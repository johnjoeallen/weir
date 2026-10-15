# Command line

```text
weir run   <file> [flow] [options]     run a flow (the `main` flow by default)
weir check <file> [--json]             check a script without running anything
weir list  <file> [--manifest]         list flows, their inputs and outputs
weir test  [path]                      run the `test` blocks of a file or folder
weir lock  [file]                      pin remote components (none are supported yet)
weir sandbox suggest <file>            not implemented yet
weir --version | --help
```

## Running a script directly

`weir FILE` with a script as the first argument is the form a shebang line uses.
A script that starts with

```text
#!/usr/bin/env -S weir --run
```

runs when executed: `./script.weir [flow] [options]`. The kernel turns that into
`weir --run ./script.weir ...`. The `-S` is needed because Linux passes everything
after the interpreter path as a single argument, and plain `env` would look for a
program literally named `weir --run`.

`--run` is required on purpose. With a plain `#!/usr/bin/env weir` line, executing
the script only shows its help and exits with return code 2, so a file never acts by accident. You
can also pass it yourself: `weir script.weir --run`. `weir run script.weir` always runs.

The other modes are subcommands only: `weir check FILE`, `weir test FILE` and
`weir list FILE`.

Typing `weir check` with no path means the `check` command. A file that happens to
have a command's name needs a path, as in `weir run ./check`. Executing it directly
is unaffected, because the kernel passes its full path.

## Options generated from inputs

Each input a flow declares becomes a command-line option of the same name.

- `in top: int = 10` becomes `--top <int>`. The default, `10`, is shown in `--help`.
- A `list<T>` input is given by repeating its option: `--tag a --tag b`.
- A `bool` input becomes a pair of switches, `--name` and `--no-name`.
- A value that cannot be read as the input's type is a usage error. `weir` exits with
  return code 2, and the message names the option.
- A `path` value is a path on the host. Weir translates it to the path inside the
  sandbox through the sandbox mounts.

## Run options

| Option | Effect |
|---|---|
| `--confirm` | also run `confirmable` steps |
| `--plan` | print what would happen and run nothing (a dry run) |
| `--json` | print one JSON result document; `print` goes to stderr |
| `--out <name>` | print one named output as raw text; `print` goes to stderr |
| `--ro <inside-path>` | turn a `rw` mount into `ro` for this run |
| `--deny-exec <name>` | refuse to start a program for this run |
| `--secret-file <name>=<path>` | read a `secret` input from a file (mode 0600) |
| `--secret-stdin <name>` | read a `secret` input from stdin |
| `--sandbox <mode>` | `required` (the default), `auto` or `off`; see [the sandbox](sandbox.md#when-the-sandbox-cannot-be-created). `WEIR_SANDBOX` sets it too |
| `--no-sandbox` | the same as `--sandbox off` |
| `--help` | show the options a script generates from its inputs |

A `secret` input is never accepted on the command line. It comes from
`WEIR_SECRET_<NAME>`, `--secret-file` or `--secret-stdin`.

## Return codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | a flow failed |
| 2 | usage error |
| 3 | `weir check` found errors |
| 4 | capability denied, including a sandbox that cannot be created |
