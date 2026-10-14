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

The script as a whole can see `/etc` and `/var/log`, both read-only. Each of its two
filters gets a smaller view: `cat_hosts` sees `/etc` but not `/var/log`, and
`count_lines` sees `/var/log` but not `/etc`. The test stubs both programs, so it
runs nothing.

```weir
#!/usr/bin/env -S weir --run

// The script as a whole can see /etc and /var/log, both read-only.
sandbox {
  mount system
  mount "/etc" ro
  mount "/var/log" ro
}

// This program can read /etc but has no view of /var/log at all.
filter cat_hosts {
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

// This one is the other way round: it can read /var/log but not /etc.
filter count_lines {
  program "wc"
  in file: path
  args ["-l", file]
  sandbox {
    see system
    see "/var/log" ro
  }
  takes none
  yields lines
}

main flow hosts {
  in  file: path = path(text: "/etc/hosts")
  out n: int

  let rows = cat_hosts(file: file)
    .filter(where: not starts_with(text: it, prefix: "#"))
  for r in rows {
    print(text: r)
  }
  print(text: "dpkg.log: ${first(of: count_lines(file: path(text: "/var/log/dpkg.log")))}")
  set n = rows.count()
}

test "counts the lines that are not comments" {
  stub cat_hosts yields lines: ["# comment", "127.0.0.1 localhost"]
  stub count_lines yields lines: ["42 /var/log/dpkg.log"]
  let r = run hosts()
  expect r.n == 1
}
```

Save it as `hosts.weir` and make it executable with `chmod +x hosts.weir`. Then:

```bash
weir check hosts.weir     # static check, nothing runs
weir test  hosts.weir     # run the test block with the programs stubbed
./hosts.weir              # run it (the shebang line supplies --run)
```

To see the restriction work, point the first filter at a file it can't see:

```bash
./hosts.weir --file /var/log/dpkg.log
# error in hosts: ExecError: `cat -- /var/log/dpkg.log` exited with code 1:
#   cat: /var/log/dpkg.log: No such file or directory
```

The file exists on the host and the script mounts it, but `cat_hosts` has no
`/var/log` in its view.

The full language is described in the [design document](DESIGN.md).
