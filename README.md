# Weir

**Documentation: <https://johnjoeallen.github.io/weir/>**

Weir is a small interpreted language for scripts: anything you would otherwise write in bash, run from a terminal, a cron job, a systemd unit or a pipeline.

Scripts have a shape: inputs, a sequence of steps, some outputs. Shell languages leave that shape implied. Weir makes it the language, and checks it before anything runs.

```weir
// examples/dpkgwatch.weir (abridged)
sandbox {
  mount system
  mount "/var/log" ro                 // the only part of the machine this script can see
}

main flow report {
  in  log: path
  in  since: duration = 7d
  in  top: int = 10
  out changes: int

  let events = load(log: log).events
    .filter(where: it.epoch >= now_epoch() - since.seconds)

  summary {
    for p in events.count_by(key: it.package).sort_by(key: it.count, order: desc).take(n: top) {
      print(text: "${p.key}\t${p.count}")
    }
  }
  set changes = events.count()
}
```

```text
$ weir run examples/dpkgwatch.weir --log /var/log/dpkg.log --since 30d
```

- **Typed inputs become the command line.** `--help` is generated, and bad values are rejected before step one (exit 2).
- **Commands take argument lists, not strings.** There is no shell, so quoting bugs cannot be expressed.
- **Every script declares the filesystem it runs in.** The `sandbox` block is enforced with Linux namespaces, including for every program the script starts. Nothing prompts.
- **`weir check`** reports problems in terms of what you wrote, with a stable code and a fix hint, before anything runs.

## Build and run

Rust 1.89 or later, no dependencies.

```text
cargo build --release          # target/release/weir
cargo test
weir run   <file> [flow] [options]
weir check <file> [--json]
weir test  [path]
weir list  <file> [--manifest]

./script.weir [flow] [options]         # with the shebang `#!/usr/bin/env -S weir --run`
./script.weir --run [flow] [options]   # without it; no --run shows the script's help and runs nothing
```

## Status

Early, but working: the interpreter runs the examples in `examples/` and the worked examples in the design document. See [docs/IMPLEMENTATION.md](docs/IMPLEMENTATION.md) for what is built, what is not, and the places where the design was silent and a decision was made. The language itself is specified in [docs/DESIGN.md](docs/DESIGN.md) and is subject to change.
