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
    The sandbox requires Linux with unprivileged user namespaces enabled. Weir has
    only been tested on Linux (x86_64). If the sandbox cannot be created, `weir run`
    exits with code 4. Passing `--no-sandbox` runs the script without a sandbox and
    prints a warning.

## Run an example

`examples/dpkgwatch.weir` summarises package activity from `/var/log/dpkg.log`:

```bash
weir run examples/dpkgwatch.weir
weir run examples/dpkgwatch.weir --log /var/log/dpkg.log.2.gz --since 30d --top 20
weir run examples/dpkgwatch.weir --help        # options generated from the script's inputs
```

## Make a script executable

A script that starts with `#!/usr/bin/env -S weir --run` runs when executed:

```bash
./examples/dpkgwatch.weir --top 5
```

With a plain `#!/usr/bin/env weir` line, executing the file only shows the
script's help and exits 2. Nothing runs by accident. See
[Command line](cli.md#running-a-script-directly) for why `-S` is needed.

## Small example

This script prints the lines of `/etc/hosts` that are not comments. It can see
`/etc` read-only and nothing else, and it has a test that stubs the `cat` program.

```weir
#!/usr/bin/env -S weir --run

sandbox {
  mount system
  mount "/etc" ro
}

filter cat_file {
  program "cat"
  in file: path
  args ["--", file]
  sandbox {
    see system
    see "/etc" ro
  }
  takes none
  yields lines
}

main flow hosts {
  in  file: path = path(text: "/etc/hosts")
  out n: int

  let rows = cat_file(file: file)
    .filter(where: not starts_with(text: it, prefix: "#"))
  for r in rows {
    print(text: r)
  }
  set n = rows.count()
}

test "counts the lines that are not comments" {
  stub cat_file yields lines: ["# comment", "127.0.0.1 localhost"]
  let r = run hosts()
  expect r.n == 1
}
```

Save it as `hosts.weir` and make it executable with `chmod +x hosts.weir`. Then:

```bash
weir check hosts.weir     # static check, nothing runs
weir test  hosts.weir     # run the test block with the program stubbed
./hosts.weir              # run it (the shebang line supplies --run)
```

The full language is described in the [design document](DESIGN.md).
