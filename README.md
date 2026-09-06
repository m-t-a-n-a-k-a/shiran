# Shiran

Shiran takes a bounded, best-effort snapshot of the Linux `/proc` and `/sys` trees as JSON.

It does not know their schemas — and does not need to.

```sh
shiran > snapshot.json
```

## Installation

Shiran is distributed as a single Linux binary.

### x86_64

```sh
curl -L https://github.com/m-t-a-n-a-k-a/shiran/releases/latest/download/shiran-x86_64-linux -o shiran && chmod +x shiran
```

### ARM64

```sh
curl -L https://github.com/m-t-a-n-a-k-a/shiran/releases/latest/download/shiran-aarch64-linux -o shiran && chmod +x shiran
```

Optionally move it somewhere in your `PATH`:

```sh
sudo mv shiran /usr/local/bin/
```

No additional package is required.

## Quick start

Capture the current `/proc` and `/sys` trees:

```sh
shiran > snapshot.json
```

Shiran writes the snapshot to standard output. Diagnostic messages are written to standard error.

## How it works

The contents of `/proc` and `/sys` vary with the running Linux system.

Shiran deliberately makes no assumptions about which paths exist or what their contents mean.

It:

1. discovers what currently exists;
2. reads what can be read safely;
3. preserves the raw contents;
4. records why anything could not be captured; and
5. emits the result as a single JSON document.

Previously unknown paths can therefore be captured without requiring a new Shiran release.

## Design principles

Shiran is intentionally small.

* No predefined schema
* No path-specific parsers
* No query language
* No continuous collection
* No interpretation
* No diagnosis
* No kernel-version-specific knowledge
* No external service

Shiran captures what `/proc` and `/sys` expose at a point in time. Interpretation is left to the consumer.

## Capture behavior

Shiran recursively walks `/proc` and `/sys`.

Symbolic links are recorded but not followed.

Files are read on a best-effort basis with bounded resource usage.

An entry may be:

* captured;
* excluded by capture policy; or
* unavailable at capture time.

Excluded and unavailable entries remain represented in the output together with a machine-readable reason.

Typical reasons include:

* size limit exceeded;
* non-text data;
* permission denied;
* entry disappeared during capture; and
* I/O error.

Entries may disappear while Shiran is running, especially under `/proc/<pid>`. This is expected and does not cause the entire snapshot to fail.

## Output

The JSON format is versioned.

The exact format may evolve before Shiran 1.0, but every snapshot includes a format version so consumers can identify incompatible changes.

Conceptually:

```json
{
  "format_version": 1,
  "proc": {
    "meminfo": {
      "status": "captured",
      "content": "MemTotal: ..."
    },
    "kcore": {
      "status": "excluded",
      "reason": "size_limit"
    },
    "12345/status": {
      "status": "unavailable",
      "reason": "vanished"
    }
  },
  "sys": {
    "class/net/eth0/mtu": {
      "status": "captured",
      "content": "1500\n"
    },
    "class/net/eth0": {
      "status": "symlink",
      "target": "../../devices/..."
    }
  }
}
```

Contents are preserved rather than interpreted.

## Compatibility

Shiran targets Linux.

It is designed to tolerate differences caused by:

* kernel versions;
* kernel configuration;
* loaded modules;
* hardware and drivers;
* containers and namespaces; and
* runtime process state.

A snapshot is best-effort. Shiran does not require every discovered entry to be readable.

## Security

Shiran snapshots may contain sensitive information.

Depending on the privileges used to run Shiran, a snapshot may include:

* process command-line arguments;
* process environment data;
* kernel and system configuration;
* device information;
* network state; and
* other application or system metadata exposed through `/proc` or `/sys`.

Treat snapshots as potentially sensitive data.

Do not publish a snapshot without reviewing its contents first.

## Contributing

Bug reports and pull requests are welcome.

## License

See [`LICENSE`](LICENSE).
