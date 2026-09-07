use serde_json::Value;
use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;
use tempfile::tempdir;

const MAX_ENTRY_BYTES: usize = 1024 * 1024;

fn field<'a>(snapshot: &'a Value, root: &str, path: &str, field: &str) -> Option<&'a str> {
    snapshot.get(root)?.get(path)?.get(field)?.as_str()
}

#[test]
fn snapshot_cases() {
    let temp = tempdir().unwrap();
    let proc_root = temp.path().join("proc");
    let sys_root = temp.path().join("sys");

    fs::create_dir_all(&proc_root).unwrap();
    fs::create_dir_all(sys_root.join("class/net/eth0")).unwrap();

    fs::write(proc_root.join("meminfo"), "MemTotal: 42 kB\n").unwrap();
    fs::write(proc_root.join("cmdline"), b"init\0quiet\0").unwrap();
    fs::write(proc_root.join("binary"), [0xff, 0xfe, 0xfd]).unwrap();
    fs::write(proc_root.join("oversized"), vec![b'x'; MAX_ENTRY_BYTES + 1]).unwrap();
    fs::write(sys_root.join("class/net/eth0/mtu"), "1500\n").unwrap();

    symlink("eth0", sys_root.join("class/net/current")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_shiran"))
        .arg("--proc-root")
        .arg(&proc_root)
        .arg("--sys-root")
        .arg(&sys_root)
        .output()
        .expect("failed to execute shiran");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let snapshot: Value = serde_json::from_slice(&output.stdout).unwrap();

    assert_eq!(snapshot["format_version"].as_u64(), Some(1));
    assert!(snapshot["execution"]["uid"].is_number());
    assert!(snapshot["execution"]["euid"].is_number());
    assert!(snapshot["execution"]["gid"].is_number());
    assert!(snapshot["execution"]["egid"].is_number());

    let cases = [
        ("meminfo status", "proc", "meminfo", "status", "captured"),
        ("meminfo content", "proc", "meminfo", "content", "MemTotal: 42 kB\n"),
        ("cmdline content", "proc", "cmdline", "content", "init\0quiet\0"),
        ("binary status", "proc", "binary", "status", "excluded"),
        ("binary reason", "proc", "binary", "reason", "binary"),
        ("oversized reason", "proc", "oversized", "reason", "size_limit"),
        ("mtu status", "sys", "class/net/eth0/mtu", "status", "captured"),
        ("mtu content", "sys", "class/net/eth0/mtu", "content", "1500\n"),
        ("symlink status", "sys", "class/net/current", "status", "symlink"),
        ("symlink target", "sys", "class/net/current", "target", "eth0"),
    ];

    for (name, root, path, key, expected) in cases {
        assert_eq!(field(&snapshot, root, path, key), Some(expected), "{name}");
    }
}

#[test]
fn cli_cases() {
    let cases: [(&str, &[&str], bool, &str, &str); 3] = [
        ("help", &["--help"], true, "Usage:", ""),
        ("version", &["--version"], true, "shiran ", ""),
        ("invalid", &["--unknown"], false, "", "--unknown"),
    ];

    for (name, args, expected_success, expected_stdout, expected_stderr) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_shiran"))
            .args(args)
            .output()
            .expect("failed to execute shiran");

        assert_eq!(output.status.success(), expected_success, "{name}");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(stdout.contains(expected_stdout), "{name}: stdout={stdout}");
        assert!(stderr.contains(expected_stderr), "{name}: stderr={stderr}");
    }
}
