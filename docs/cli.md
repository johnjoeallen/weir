# Command line

```text
weir run   <file> [flow] [--flags]     run a flow (the `main` flow by default)
weir check <file> [--json]             check a script without running anything
weir list  <file> [--manifest]         list flows, their inputs and outputs
weir test  [path]                      run the `test` blocks of a file or folder
weir lock  [file]                      pin remote components (none are supported yet)
weir sandbox suggest <file>            not implemented yet
weir --version | --help
```

## Running a script directly

`weir FILE` with a script as the first argument is the form a shebang line uses.

```text
./script.weir --run [flow] [--flags]    run
./script.weir --check | --test | --list other modes
./script.weir                           no mode flag: show the script's help, run nothing (exit 2)
```

A script can't be named `check`, `test`, `list`, `run`, `lock`, `sandbox` or
`help`; use `./check` or `weir run check` for such a file.

## Generated flags

Typed inputs become flags. `in top: int = 10` is `--top <int>` with `10` shown as
the default. A `list<T>` input takes repeated flags, a `bool` is
`--flag` / `--no-flag`, and a value that does not parse as its type is exit 2 with
the flag named. A `path` is a host path, translated through the sandbox mounts.

## Run flags

| Flag | Effect |
|---|---|
| `--confirm` | also run `confirmable` steps |
| `--plan` | print what would happen and run nothing (a dry run) |
| `--json` | print one JSON result document; `print` goes to stderr |
| `--out <name>` | print one named output as raw text; `print` goes to stderr |
| `--ro <inside-path>` | turn a `rw` mount into `ro` for this run |
| `--deny-exec <name>` | refuse to start a program for this run |
| `--secret-file <name>=<path>` | read a `secret` input from a file (mode 0600) |
| `--secret-stdin <name>` | read a `secret` input from stdin |
| `--no-sandbox` | run without the filesystem sandbox (prints a warning) |
| `--help` | show the flags a script generates from its inputs |

A `secret` input is never accepted on the command line. It comes from
`WEIR_SECRET_<NAME>`, `--secret-file` or `--secret-stdin`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | a flow failed |
| 2 | usage error |
| 3 | `weir check` found errors |
| 4 | capability denied, including a sandbox that cannot be created |
