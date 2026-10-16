# Implementation Status

This describes the reference interpreter in `src/`, measured against [DESIGN.md](DESIGN.md). It is Rust with no dependencies, and Linux is the only platform on which it has been run.

Tests: `cargo test` runs 21 unit tests (`src/tests.rs`) and 35 integration tests (`tests/cli.rs`) that drive the real binary. The integration tests include the two worked examples in the design document, which must check clean, and a sandbox test that proves what a program inside can and cannot see. That test is skipped, not failed, on a host that cannot create the sandbox.

## What is built

| Design section | State |
|---|---|
| 2. Execution model | `run`, `check`, `list`, `test`. `run` always checks first. Exit codes 0 to 4 as specified. `check --json`. |
| 3. Declarations | `flow`, `tool`, `fn`, `test`. |
| 4. Flow anatomy | `in`, `out`, `requires`, `lock`, `finally`; steps with `when`, `confirmable`, `as` labels; the repeated-step label rule (`E-STEP-LABEL`, `E-DUP-STEP`). |
| 5. Types | All types parse. Values are checked against their declared type at run time (`let x: T`, flow and fn inputs, outputs, `set`). `map<K,V>` exists as a type only: there is no map literal. |
| 6. Commands and errors | Argument lists, no shell. `ExecError`, `CapabilityError`, `IoError`, `ParseError`, typed `catch`, `try` as an attempt record, `fail`. `Timeout` is reserved and never thrown. |
| 7. Sandbox | Real: user + mount + PID namespaces, a tmpfs root populated with bind mounts, `pivot_root`, private `/tmp`, minimal `/dev`, `/proc`. Host-to-inside path translation, `ro`/`rw` enforcement (also reported by the interpreter with the mount named), `see` lines on a tool and `within` blocks, both narrowing in a nested mount namespace, `--ro`, `--deny-exec`, `--sandbox required|auto|off` (also `WEIR_SANDBOX`; `--no-sandbox` is `off`). Exit 4 if it cannot be created, unless the mode is `auto`. Static checks `E-SANDBOX-EXEC-UNREACHABLE`, `E-SANDBOX-LOCK-UNSHARED`, `E-SANDBOX-NOT-ENTRY`, `E-SEE-UNMOUNTED`, `W-NO-SANDBOX`. |
| 8. Calls | Named arguments only; `W-DEFAULT-ARG` for literal defaults, with `// weir:allow CODE`. |
| 9. Pipelines | `lines map filter take unique count count_by sort_by`, declared tools as stages, `it`. |
| 10. Components | Local `use "./path.weir"` and `use "./path.weir#flow"`. Private flows are not importable. |
| 11. CLI | Flags generated from inputs, `--help`, `--confirm`, `--plan`, `--json`, `--out`, secrets from `WEIR_SECRET_<NAME>`, `--secret-file` (mode checked) and `--secret-stdin`; a secret on argv is exit 2. |
| 12. Testing | `fixture dir`, `stub`, `expect`, `run <flow>(...)`. |
| 7.3.1 Scoped views | `within <items> { ... }` as a statement or a value, `return`, nesting by intersection, built-ins held to the view (`exists` is `false`), `E-WITHIN-NO-RETURN`, `E-FN-IMPURE`. A tool writes `see` lines directly; the old `sandbox { see }` form in a tool is `E-TOOL-SANDBOX`. |
| 7.1.1 Explicit mounts | `mount ... explicit`: held out of the default view, reached only by a `see` line or `within` header that names it. Built-ins get a `CapabilityError` (`exists` is `false`). |
| 15, 16. Grammar, reserved words | As sketched, plus the additions below. |

Checker rules beyond those named above: unknown names, calls, stages and arguments with "did you mean" hints; missing, duplicate and unknown arguments; literal argument and default type mismatches; `it` outside a stage; assignment to a `let`; optional values used without `if let`; `secret` values used outside `env`/stdin; `fn` purity; `requires` purity; `W-SHELL-STRING`, `W-DESTRUCTIVE-PROGRAM`, `E-DYNAMIC-CMD`; tools that cannot start or join a pipeline; `catch` of a name that is not an error type. A failed import suppresses the follow-on "unknown name" errors it would cause.

## What is not built

- **Remote components and the lockfile.** `use "github:..."` and `use "https://..."` are reported as `E-USE-REMOTE-UNSUPPORTED`; `weir lock` has nothing to pin. `examples/logwatch.weir` imports a local `examples/parse_access.weir` instead (the remote component it once named does not exist), so both examples check, test and run.
- **`weir sandbox suggest`.**
- **Streaming pipelines (9.2).** Every stage materialises its input into a list, so `take` does not stop the upstream program and the log is held in memory. The broken-pipe question in 19.5 does not arise yet.
- **A complete `--plan` (19.5).** `--plan` is a dry run: reads happen, writes and commands are printed and not executed, and a command yields an empty result. A branch that depends on a command's output can therefore differ from a real run, and a `fail` stops the plan.
- **Static type inference.** The checker does not infer types through pipelines or `let`, so a type error that depends on a computed value is found at run time with a message naming the value and the expected type.
- **Deeper `secret` analysis.** Direct uses are caught. A secret passed through a `fn` return value or a record field is not tracked.
- **A static musl binary** (the design asks for one). The release binary links the host libc dynamically. Everything the sandbox needs from libc is loaded before the pivot, but this is not the guarantee the design describes.
- **Non-Linux sandbox.** The module is gated by `cfg(target_os = "linux")` and other platforms exit 4. Other platforms have not been built.
- **Network confinement** is outside the design's v1.

## Decisions where the design was silent or inconsistent

These are the places a reader of the design could reasonably have expected something else.

1. **`if` as an expression.** `examples/logwatch.weir` writes `let raw: list<string> = if ... { a } else { b }`, which the grammar sketch omits. It is supported: `else` is required and a branch's value is its last expression.
2. **Sandbox `in` paths may be strings.** The design's own example is `in logs: path = "/var/log/nginx"` in the sandbox block, but flow inputs write `path(text: "...")` because a string is never implicitly a path. A sandbox `in` is a host path evaluated before the sandbox exists, so a string literal is accepted there only. Each runnable flow must declare the same `in` again (same type, no default) and receives the inside path; the command-line option is shared.
3. **A flow call's value is a record of its outputs**, so `load(...).entries` and `double(n: 4).r` work.
4. **Outputs are write-only.** `set` assigns; reading an output name is `E-UNKNOWN-NAME` with a hint to keep the value in a `var`.
5. **`try expr` yields `{ok, code, value, message}`.** `fail` is not catchable by `catch`, but `try` captures it.
6. **Operator precedence follows 15 literally:** unary `not` binds tighter than comparison, so `not a == b` means `(not a) == b`.
7. **`with_suffix(text: ".bak")` appends** to the file name (`deny.conf` becomes `deny.conf.bak`), rather than replacing the extension.
8. **`run(cmd:, args:)` yields lines**, and takes an optional `ok_codes`.
9. **Name resolution order for a stage:** native stages, then declared tools, then a built-in applied to the value (`path.with_suffix(...)`). A declared tool, fn or flow cannot reuse a built-in or native-stage name (`E-SHADOWS-BUILTIN`).
10. **`first` and `nth` out of range, and `int(text: "abc")`, throw `ParseError`.**
11. **Tests never run real programs.** A tool without a `stub` fails the test with the line to add. A stub's value is checked against the tool's `yields`. A flow that fails inside `run` fails the test unless it is wrapped in `try`. Fixtures are directories of text files, mounted read-write at their stated path. A stub may name arguments (`stub compose(words: [...]) yields lines: ...`) and then answers only matching calls; the matching stubs for a tool answer in turn and the last repeats. `E-STUB-ARG` for an unknown input name.
12. **No `sandbox` block means unconfined,** as in 7.6, modelled as a single read-write `/` mount. Built-ins then act on host paths.
13. **The checker reads `PATH`.** 2.1 says `check` reads nothing but source files, while 7.2 needs a program's resolved path to test it against the mounts. It resolves programs on this machine; a program that is absent is `W-PROGRAM-NOT-FOUND`, not an error, so a script can be checked on a machine that is not the target.
14. **The sandbox is entered in-process, not by re-executing `weir __sandbox-run`.** The process is single-threaded at that point, so `unshare(CLONE_NEWUSER)` is allowed. The observable behaviour is the same. The original process stays on the host to wait, forward `SIGHUP`/`SIGINT`/`SIGTERM`, return the child's exit status and remove the temporary root.
15. **The mount namespace is created in the child, not the parent.** `pivot_root` changes the root of every process in the namespace, so a shared namespace dragged the parent off the host and left a temporary directory behind. This was found by a test.
16. **Step identity.** A flow call is named after the flow, an inline block after its name, and a non-flow expression statement that carries `when` or `confirmable` is `line N`.
17. **Imported flows run under the entry file's sandbox,** and resolve their own tools and fns within their own file.
18. **Test stubs are named in the entry file.** A `stub` can only name a tool declared in the file the test is in, and `lock` is not taken in test mode. A component that must be testable from its importer should not depend on a program; `parse_access.weir` does its date arithmetic in pure `fn`s for that reason (and to avoid one process per log line).
19. **A `within` block builds one narrowed root per program,** not one per block. It costs about 0.6 ms per program (200 `run` calls: 0.18 s inside a block, 0.05 s outside), so sharing a root through `setns` was not worth the complexity. An item in a tool's own `see` lines is cut down to the view by intersection; `system` only if both have it.

20. **`--sandbox auto` probes first.** Falling back after the real `enter` has failed is not possible: `unshare(CLONE_NEWPID)` has by then changed what this process's children become, and a failure in the child comes too late. So `auto` forks a throwaway child that builds and pivots into an empty root and reports the reason through a pipe (`sandbox::probe`, about a millisecond). Only if that succeeds does the real `enter` run, and `required` skips the probe.
21. **Explicit mounts are bound under `/.weir-explicit/N`,** not at their inside path, so a recursive bind of a parent folder cannot carry them along. With any explicit mount, every program is started in a narrowed root (the default view, or its own `see` lines, or the `within` view) and the narrowing translates a requested inside path back to the holding path. The interpreter's mount table maps an explicit entry's host side to the holding path, so built-ins that are allowed (inside a requesting `within`) work unchanged. Open: no warning yet when a broader mount exposes the same host folder.
22. **A mount of the host's root is an error with no override** (`E-MOUNT-ROOT`, and exit 2 when a sandbox input resolves to `/`). A script that needs the whole machine uses `--sandbox off`. Outside the sandbox, `see` lines and `within` blocks still build a view for each program: `sandbox::narrow` then creates a user namespace as well as a mount namespace in the child (the current user mapped to itself), and the empty `/tmp/.weir-see-<pid>` folder it needs on the host is removed after the program exits. After an `auto` fallback the views are skipped, because the probe has already shown that namespaces are unavailable. A script with no `sandbox` block counts as having the system set mounted, so `see system` works there.
23. **Making a `ro` tree read-only copes with locked submounts.** Namespace files (`nsfs`) and kernel pseudo-filesystems that refuse the remount are skipped; any other locked submount is covered with an empty read-only tmpfs, so it shows as empty and is never left writable; one whose path the user cannot walk is left alone. A mount whose inside path is `/` is built without a trailing slash, which had made its read-only remount silently do nothing.

## Known limits of the sandbox

- The interpreter is PID 1 of the new PID namespace but reaps only the children it starts. A grandchild orphaned onto it stays a zombie until the run ends.
- A program's environment is rebuilt (`PATH`, `LANG`, `TZ`, `HOME=/tmp` and declared `env`), but `PATH` keeps the host's value, so it can name directories that do not exist inside.
- A `ro` mount made recursively is remounted read-only including its submounts. Each submount keeps the flags the host gave it.
- A tool with `see` lines, or a program started inside a `within` block, runs in a nested mount namespace under `/tmp/.weir-see-<pid>`; an empty directory of that name remains in the sandbox's private `/tmp` until the run ends.
- Verified on Linux 6.12 x86_64 with unprivileged user namespaces enabled. The `aarch64` `pivot_root` syscall number is present and untested.
- Nested flow calls are limited to 64 levels and nested fn calls to 200 (one shared depth counter), so a runaway recursive script fails with an error instead of overflowing the stack.

## Source layout

```text
src/lexer.rs     tokens; drops newlines that continue an expression
src/parser.rs    hand-written recursive descent; first error only, so no cascades
src/ast.rs       syntax tree
src/loader.rs    entry file and local `use` imports
src/check.rs     weir check: names, arguments, purity, sandbox declarations, lints
src/interp.rs    tree-walking interpreter: flows, steps, pipelines, tools, programs
src/builtins.rs  built-in signatures (shared with the checker) and pure implementations
src/mounts.rs    the mount table and host/inside path translation
src/sandbox.rs   namespaces, pivot_root, per-tool narrowing (Linux only)
src/cli.rs       commands, generated flags, test runner
src/value.rs, json.rs, diag.rs, error.rs, pretty.rs
```
