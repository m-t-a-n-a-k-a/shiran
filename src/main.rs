#[cfg(not(target_os = "linux"))]
compile_error!("Shiran supports Linux only.");

use clap::Parser;
use libc::{O_NOFOLLOW, O_NONBLOCK};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const FORMAT_VERSION: u32 = 1;
const MAX_ENTRY_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "shiran",
    version,
    about = "A bounded, schema-less snapshot of Linux /proc and /sys as JSON."
)]
struct Cli {
    #[arg(long, default_value = "/proc", value_name = "PATH")]
    proc_root: PathBuf,

    #[arg(long, default_value = "/sys", value_name = "PATH")]
    sys_root: PathBuf,
}

#[derive(Debug)]
struct Roots {
    proc: PathBuf,
    sys: PathBuf,
}

impl From<Cli> for Roots {
    fn from(cli: Cli) -> Self {
        Self {
            proc: cli.proc_root,
            sys: cli.sys_root,
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct ExecutionContext {
    uid: u32,
    euid: u32,
    gid: u32,
    egid: u32,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct Limits {
    max_entry_bytes: usize,
    max_total_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entry_bytes: MAX_ENTRY_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Reason {
    Binary,
    SizeLimit,
    TotalLimit,
    SpecialFile,
    PermissionDenied,
    Vanished,
    WouldBlock,
    IoError,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Entry {
    Captured { content: String },
    Excluded { reason: Reason },
    Unavailable { reason: Reason },
    Symlink { target: String },
}

#[derive(Debug, Serialize)]
struct Snapshot {
    format_version: u32,
    execution: ExecutionContext,
    limits: Limits,
    proc: BTreeMap<String, Entry>,
    sys: BTreeMap<String, Entry>,
}

struct Budget {
    remaining: usize,
}

impl Budget {
    fn new(bytes: usize) -> Self {
        Self { remaining: bytes }
    }

    fn exhausted(&self) -> bool {
        self.remaining == 0
    }

    fn allowance(&self, max_entry_bytes: usize) -> usize {
        self.remaining.min(max_entry_bytes)
    }

    fn consume(&mut self, bytes: usize) {
        self.remaining = self.remaining.saturating_sub(bytes);
    }
}

struct ReadResult {
    bytes: Vec<u8>,
    exceeded_limit: bool,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,

        Err(message) => {
            eprintln!("shiran: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let snapshot = make_snapshot(&Roots::from(cli), Limits::default())?;

    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    serde_json::to_writer(&mut stdout, &snapshot)
        .map_err(|error| format!("failed to write JSON: {error}"))?;

    writeln!(stdout).map_err(|error| format!("failed to write output: {error}"))
}

fn make_snapshot(roots: &Roots, limits: Limits) -> Result<Snapshot, String> {
    let execution = read_execution_context()?;
    let mut budget = Budget::new(limits.max_total_bytes);

    let proc = capture_root(&roots.proc, limits, &mut budget)
        .map_err(|error| format!("failed to snapshot {}: {error}", roots.proc.display()))?;

    let sys = capture_root(&roots.sys, limits, &mut budget)
        .map_err(|error| format!("failed to snapshot {}: {error}", roots.sys.display()))?;

    Ok(Snapshot {
        format_version: FORMAT_VERSION,
        execution,
        limits,
        proc,
        sys,
    })
}

fn read_execution_context() -> Result<ExecutionContext, String> {
    let status = fs::read_to_string("/proc/self/status")
        .map_err(|error| format!("failed to read /proc/self/status: {error}"))?;

    parse_execution_context(&status)
        .ok_or_else(|| "invalid /proc/self/status".to_string())
}

fn parse_execution_context(status: &str) -> Option<ExecutionContext> {
    let (uid, euid) = parse_ids(status, "Uid:")?;
    let (gid, egid) = parse_ids(status, "Gid:")?;

    Some(ExecutionContext {
        uid,
        euid,
        gid,
        egid,
    })
}

fn parse_ids(status: &str, key: &str) -> Option<(u32, u32)> {
    let line = status.lines().find(|line| line.starts_with(key))?;
    let mut values = line[key.len()..].split_whitespace();

    let real = values.next()?.parse().ok()?;
    let effective = values.next()?.parse().ok()?;

    Some((real, effective))
}

fn capture_root(
    root: &Path,
    limits: Limits,
    budget: &mut Budget,
) -> io::Result<BTreeMap<String, Entry>> {
    let metadata = fs::symlink_metadata(root)?;

    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a directory: {}", root.display()),
        ));
    }

    let mut entries = BTreeMap::new();

    walk_directory(root, root, limits, budget, &mut entries, true)?;

    Ok(entries)
}

fn walk_directory(
    root: &Path,
    directory: &Path,
    limits: Limits,
    budget: &mut Budget,
    output: &mut BTreeMap<String, Entry>,
    is_root: bool,
) -> io::Result<()> {
    let children = match directory_children(directory) {
        Ok(children) => children,

        Err(error) if is_root => return Err(error),

        Err(error) => {
            output.insert(
                relative_key(root, directory),
                Entry::Unavailable {
                    reason: io_reason(&error),
                },
            );

            return Ok(());
        }
    };

    for path in children {
        capture_path(root, &path, limits, budget, output)?;
    }

    Ok(())
}

fn directory_children(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut children = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;

    children.sort();

    Ok(children)
}

fn capture_path(
    root: &Path,
    path: &Path,
    limits: Limits,
    budget: &mut Budget,
    output: &mut BTreeMap<String, Entry>,
) -> io::Result<()> {
    let key = relative_key(root, path);

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,

        Err(error) => {
            output.insert(
                key,
                Entry::Unavailable {
                    reason: io_reason(&error),
                },
            );

            return Ok(());
        }
    };

    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        output.insert(key, capture_symlink(path));
        return Ok(());
    }

    if file_type.is_dir() {
        return walk_directory(root, path, limits, budget, output, false);
    }

    if file_type.is_file() {
        output.insert(key, capture_file(path, &metadata, limits, budget));
        return Ok(());
    }

    output.insert(
        key,
        Entry::Excluded {
            reason: Reason::SpecialFile,
        },
    );

    Ok(())
}

fn capture_symlink(path: &Path) -> Entry {
    match fs::read_link(path) {
        Ok(target) => Entry::Symlink {
            target: target.to_string_lossy().into_owned(),
        },

        Err(error) => Entry::Unavailable {
            reason: io_reason(&error),
        },
    }
}

fn capture_file(
    path: &Path,
    metadata: &Metadata,
    limits: Limits,
    budget: &mut Budget,
) -> Entry {
    if metadata.len() > limits.max_entry_bytes as u64 {
        return Entry::Excluded {
            reason: Reason::SizeLimit,
        };
    }

    if budget.exhausted() {
        return Entry::Excluded {
            reason: Reason::TotalLimit,
        };
    }

    if metadata.len() > 0 && metadata.len() > budget.remaining as u64 {
        return Entry::Excluded {
            reason: Reason::TotalLimit,
        };
    }

    let allowance = budget.allowance(limits.max_entry_bytes);

    let read = match read_bounded(path, allowance) {
        Ok(read) => read,

        Err(error) => {
            return Entry::Unavailable {
                reason: io_reason(&error),
            };
        }
    };

    // Reads count against the total budget even when the entry is later
    // excluded as binary or oversized.
    budget.consume(read.bytes.len());

    if read.exceeded_limit {
        return Entry::Excluded {
            reason: if allowance < limits.max_entry_bytes {
                Reason::TotalLimit
            } else {
                Reason::SizeLimit
            },
        };
    }

    match decode_text(read.bytes) {
        Ok(content) => Entry::Captured { content },
        Err(reason) => Entry::Excluded { reason },
    }
}

fn read_bounded(path: &Path, limit: usize) -> io::Result<ReadResult> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK | O_NOFOLLOW)
        .open(path)?;

    // One extra byte tells us whether the content exceeded the limit.
    let mut reader = file.take((limit as u64).saturating_add(1));
    let mut bytes = Vec::with_capacity(limit.min(8192).saturating_add(1));

    reader.read_to_end(&mut bytes)?;

    Ok(ReadResult {
        exceeded_limit: bytes.len() > limit,
        bytes,
    })
}

fn decode_text(bytes: Vec<u8>) -> Result<String, Reason> {
    let text = String::from_utf8(bytes).map_err(|_| Reason::Binary)?;

    if text.chars().all(is_text_character) {
        Ok(text)
    } else {
        Err(Reason::Binary)
    }
}

fn is_text_character(character: char) -> bool {
    !character.is_control() || matches!(character, '\0' | '\n' | '\r' | '\t')
}

fn io_reason(error: &io::Error) -> Reason {
    match error.kind() {
        io::ErrorKind::PermissionDenied => Reason::PermissionDenied,
        io::ErrorKind::NotFound => Reason::Vanished,
        io::ErrorKind::WouldBlock => Reason::WouldBlock,
        _ => Reason::IoError,
    }
}

fn relative_key(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS_VALID: &str =　"Name:\tshiran\nUid:\t1000\t1001\t1002\t1003\nGid:\t2000\t2001\t2002\t2003\n";
    const STATUS_NO_UID: &str = "Gid:\t2000\t2000\t2000\t2000\n";
    const STATUS_NO_GID: &str = "Uid:\t1000\t1000\t1000\t1000\n";
    const STATUS_BAD_UID: &str =　"Uid:\tbad\t1000\t1000\t1000\nGid:\t2000\t2000\t2000\t2000\n";

    fn context(uid: u32, euid: u32, gid: u32, egid: u32) -> ExecutionContext {
        ExecutionContext {
            uid,
            euid,
            gid,
            egid,
        }
    }

    fn text(value: &str) -> Result<String, Reason> {
        Ok(value.to_string())
    }

    #[test]
    fn execution_context_cases() {
        let cases = [
            ("valid", STATUS_VALID, Some(context(1000, 1001, 2000, 2001))),
            ("missing uid", STATUS_NO_UID, None),
            ("missing gid", STATUS_NO_GID, None),
            ("invalid uid", STATUS_BAD_UID, None),
        ];

        for (name, input, expected) in cases {
            assert_eq!(parse_execution_context(input), expected, "{name}");
        }
    }

    #[test]
    #[rustfmt::skip]
    fn text_cases() {
        let cases: [(&str, &[u8], Result<String, Reason>); 5] = [
            ("plain", b"hello\nworld\n", text("hello\nworld\n")),
            ("nul separated", b"shiran\0--help\0", text("shiran\0--help\0")),
            ("empty", b"", text("")),
            ("control", b"hello\x01world", Err(Reason::Binary)),
            ("invalid utf8", &[0xff, 0xfe, 0xfd], Err(Reason::Binary)),
        ];

        for (name, input, expected) in cases {
            assert_eq!(decode_text(input.to_vec()), expected, "{name}");
        }
    }

    #[test]
    fn io_reason_cases() {
        let cases = [
            (io::ErrorKind::PermissionDenied, Reason::PermissionDenied),
            (io::ErrorKind::NotFound, Reason::Vanished),
            (io::ErrorKind::WouldBlock, Reason::WouldBlock),
            (io::ErrorKind::InvalidData, Reason::IoError),
        ];

        for (input, expected) in cases {
            let error = io::Error::from(input);
            assert_eq!(io_reason(&error), expected, "{input:?}");
        }
    }

    #[test]
    fn reason_serialization_cases() {
        let cases = [
            (Reason::Binary, "binary"),
            (Reason::SizeLimit, "size_limit"),
            (Reason::TotalLimit, "total_limit"),
            (Reason::SpecialFile, "special_file"),
            (Reason::PermissionDenied, "permission_denied"),
            (Reason::Vanished, "vanished"),
            (Reason::WouldBlock, "would_block"),
            (Reason::IoError, "io_error"),
        ];

        for (input, expected) in cases {
            let value = serde_json::to_value(input).unwrap();
            assert_eq!(value.as_str(), Some(expected), "{input:?}");
        }
    }

    #[test]
    #[rustfmt::skip]
    fn cli_cases() {
        let cases: [(&str, &[&str], &str, &str); 2] = [
            ("defaults", &["shiran"], "/proc", "/sys"),
            ("overrides", &["shiran", "--proc-root", "p", "--sys-root", "s"], "p", "s"),
        ];

        for (name, args, expected_proc, expected_sys) in cases {
            let cli = Cli::try_parse_from(args.iter().copied()).unwrap();

            assert_eq!(cli.proc_root, PathBuf::from(expected_proc), "{name}");
            assert_eq!(cli.sys_root, PathBuf::from(expected_sys), "{name}");
        }
    }
}
