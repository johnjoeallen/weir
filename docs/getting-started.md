# Getting started

## Install

### From source

Rust 1.89 or later. Weir has no dependencies.

```bash
git clone https://github.com/johnjoeallen/weir
cd weir
cargo build --release        # target/release/weir
cargo test
```

### As a Debian package

```bash
cargo install cargo-deb
scripts/build-deb.sh         # builds target/debian/weir_*.deb, then bumps the build number
sudo apt install ./target/debian/weir_*.deb
```

The binary is installed as `/usr/bin/weir`. The docs and the example scripts go
under `/usr/share/doc/weir/`. The package links the host libc dynamically.

!!! note "Platform"
    The sandbox needs Linux with unprivileged user namespaces. Weir has only been
    run on Linux (x86_64). On a host that cannot create the sandbox, `weir run`
    exits 4, and `--no-sandbox` runs without it and prints a warning.

## Run an example

`examples/dpkgwatch.weir` summarises package activity from `/var/log/dpkg.log`:

```bash
weir run examples/dpkgwatch.weir
weir run examples/dpkgwatch.weir --log /var/log/dpkg.log.2.gz --since 30d --top 20
weir run examples/dpkgwatch.weir --help        # flags generated from the script's inputs
```

## Make a script executable

A script can start with `#!/usr/bin/env -S weir --run`. Executing it then runs it:

```bash
./examples/dpkgwatch.weir --top 5
./examples/dpkgwatch.weir --check
```

With a plain `#!/usr/bin/env weir` line, or no mode flag, executing the file only
shows the script's help and exits 2. Nothing runs by accident.

## Write one

```weir
sandbox {
  mount system
  mount "/etc" ro
}

filter cat_file {
  program "cat"
  in file: path
  args ["--", file]
  sandbox { see system  see "/etc" ro }
  takes none
  yields lines
}

main flow hosts {
  in  match: string?
  out n: int

  let rows = cat_file(file: path(text: "/etc/hosts")).filter(where: not starts_with(text: it, prefix: "#"))
  for r in rows { print(text: r) }
  set n = rows.count()
}

test "counts non-comment lines" {
  stub cat_file yields lines: ["# comment", "127.0.0.1 localhost"]
  let r = run hosts()
  expect r.n == 1
}
```

```bash
weir check hosts.weir
weir test  hosts.weir
weir run   hosts.weir
```

The full language is described in the [design document](DESIGN.md).
