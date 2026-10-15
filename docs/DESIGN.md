# Weir: Design Document

**Status: early design, with a reference interpreter.** The language, its syntax and its tooling are all subject to change, and several sections end in open questions. This document is the specification. [IMPLEMENTATION.md](IMPLEMENTATION.md) records what the interpreter in `src/` implements, what it does not, and the decisions made where this document was silent.

## Introduction

Scripts that glue programs together are still mostly written in bash. Bash is everywhere, but both people and AI models make the same predictable mistakes in it: unquoted expansions, word splitting, `rm -rf "$DIR/"` with an empty `$DIR`, forgotten `set -e`, pipelines that hide failures, secrets on the command line, and scripts that prompt and hang under cron. Almost all of these are detectable by static analysis, but bash's grammar makes that analysis unreliable, and the mistakes are legal programs.

CI workflow files (GitHub Actions and similar) have a better structure: typed inputs, named steps, declared outputs. They then wrap that structure around YAML and shell strings, which loses the safety the structure could have provided.

Weir is an interpreted language for scripts that takes the structural shape of a workflow file and makes it the language itself. It targets anything otherwise written in bash and run from a terminal, a cron job, a systemd unit or a CI pipeline. CI is one use case, not the model. Its design goal is that the mistakes above cannot be expressed, or are rejected by `weir check` before anything runs, with an error message precise enough for a person or a model to fix without guessing.

Non-goals: Weir is not a general-purpose language, not an interactive shell, and not a build system. It does not try to be fast at computation.

## Table of Contents

1. [Design Principles](#1-design-principles)
2. [Execution Model](#2-execution-model)
3. [Declarations](#3-declarations)
4. [Flow Anatomy](#4-flow-anatomy)
5. [Types and Values](#5-types-and-values)
6. [Commands and Errors](#6-commands-and-errors)
7. [Capabilities and the Filesystem Sandbox](#7-capabilities)
8. [Calls and Naming](#8-calls-and-naming)
9. [Pipelines](#9-pipelines)
10. [Components](#10-components)
11. [CLI Surface](#11-cli-surface)
12. [Testing](#12-testing)
13. [Worked Example: Log Analysis and Config Change](#13-worked-example-log-analysis-and-config-change)
14. [Worked Example: Certificate Expiry Checker](#14-worked-example-certificate-expiry-checker)
15. [Grammar Sketch](#15-grammar-sketch)
16. [Reserved Words](#16-reserved-words)
17. [Comparison to Other Tools](#17-comparison-to-other-tools)
18. [Implementation Notes](#18-implementation-notes)
19. [Open Questions and Known Weaknesses](#19-open-questions-and-known-weaknesses)

## 1. Design Principles

1. **The author's source is what runs.** No compile step, no generated intermediate script. Every error refers to text the author wrote.
2. **Make the common mistake unrepresentable first, detectable second, and a runtime failure third.** Each rule in this document is tagged by which of the three it relies on where that matters.
3. **Failure is loud by default.** Tolerating failure is an explicit act.
4. **Nothing prompts unless asked.** A script that works at a terminal must behave identically under cron.
5. **Effects are declared, and enforced.** The top of the entry file defines the filesystem the script runs in: which host folders are visible, where, and in which mode. The interpreter and every program it starts live inside that sandbox, so what a script can touch can be read from one place. Which programs it may start is inferred. The network is not covered.
6. **Error messages are the product.** See [section 19](#19-open-questions-and-known-weaknesses).

## 2. Execution Model

Weir is a single interpreter binary. `weir run file.weir` parses and interprets the file directly. There is no ahead-of-time compile step and no generated code in any other language. This is a deliberate rejection of the "transpile to bash" approach: a transpiled script produces errors that refer to generated lines the author never wrote, and it inherits bash's quoting hazards at the output layer.

### 2.1 Subcommands that matter for the model

**`weir check <file>`** is a static pass. It parses, resolves names, type-checks, infers the executable set, checks it against the declared sandbox, and runs lint rules. It never runs a command or reads the filesystem beyond the source files and the lockfile. Each finding has:

| Field  | Meaning                                    |
|--------|--------------------------------------------|
| `file` | path of the source file                    |
| `line` | 1-based line                               |
| `col`  | 1-based column                             |
| `code` | stable identifier such as `W-DEFAULT-ARG`  |
| `msg`  | one-sentence description                   |
| `fix`  | a concrete suggestion, when one exists     |

Text mode prints `file:line:col: code: message` followed by the fix hint. `--json` prints an array of those objects. Codes are stable across releases; messages are not.

**`weir run --plan <file> [flow] [flags]`** prints every command, file write and file delete that would occur, and executes nothing. See [section 19](#19-open-questions-and-known-weaknesses) for the limit on what a plan can know.

### 2.2 Exit codes

| Code | Meaning                                                              |
|------|----------------------------------------------------------------------|
| 0    | success                                                              |
| 1    | flow failure (a step failed, an uncaught error, `fail`)              |
| 2    | usage error (bad flag, missing input, wrong type on the command line) |
| 3    | check error (`weir check` found errors, or `run` refused to start)   |
| 4    | capability denied (the sandbox cannot be created, or a filter's `see` clause asks for more than the sandbox provides) |

`weir run` always performs the check pass first. Check errors exit 3 and nothing runs. Warnings do not block a run.

Rationale for distinguishing 2, 3 and 4: callers such as cron wrappers and CI need to tell "the script ran and failed" from "the script was invoked wrongly" from "the script is broken" from "the script tried to exceed its grant". Collapsing these into 1 is what bash does, and it forces log scraping.

## 3. Declarations

There are four declarations, and only four. A file contains `use` imports and any number of these.

### 3.1 `flow`

A named, typed unit of work. A file's entry point is marked `main`. A file containing a single flow needs no marker. `private` flows are callable only from within their own file, never from the CLI or another file.

```weir
main flow rotate {
  in dir: path
  // ...
}

private flow prune_old {
  in dir: path
  // ...
}
```

### 3.2 `filter`

A typed wrapper around exactly one external program. It declares the program, its parameters, how it consumes input (`takes`), what it produces (`yields`), and which exit codes mean success (`ok_codes`).

```weir
filter journalctl {
  program "journalctl"
  in unit: string
  in since: string = "24 hours ago"
  args ["--no-pager", "-u", unit, "--since", since]
  takes none
  yields lines
  ok_codes [0]
}
```

`takes` and `yields` are each one of `none`, `lines`, `text`, `json`. `takes none` means the program's stdin is closed (`/dev/null`). `ok_codes` defaults to `[0]`.

The `program` is a string literal. It must be literal so the checker can infer executable capabilities ([section 7](#7-capabilities)).

### 3.3 `fn`

Pure computation. A `fn` cannot run commands, access files or the network, read the clock, or call a non-pure flow. The checker enforces this by restricting a `fn` body to other `fn`s and pure built-ins.

```weir
fn ip_of(line: string) -> string {
  return first(of: split(text: line, on: " "))
}
```

### 3.4 `test`

Runs under `weir test` against fixtures. See [section 12](#12-testing).

### 3.5 Why only four

Every additional declaration kind (type definitions, aliases, macros, classes) widens the surface a model or a person has to hold in mind, and each pulls toward a general-purpose language. Reusable data shapes are handled by structural record types written inline ([section 5](#5-types-and-values)). That is itself a concession and is listed as an open question.

**Alternative rejected: a single `fn` kind that may or may not be pure.** The purity split is what lets the checker promise that a `fn` call in a `--plan` run can be evaluated safely, and lets tests call a `fn` without fixtures.

**Alternative rejected: wrapping programs ad hoc with `run` only.** `run` still exists, but a `filter` gives the checker a declared stdin/stdout shape and exit-code contract, which is what pipeline typing depends on.

## 4. Flow Anatomy

```weir
main flow example {
  in    name: string                 // required input
  in    count: int = 3               // optional, with default
  in    note: string?                // optional, absent if not given
  in    token: secret                // credential, never accepted on argv
  out   report: path
  requires exists(path: "/etc/example")
  lock "/run/lock/example.lock"     // an inside path; must be under a rw mount

  prepare {                          // inline step
    // ...
  }
  other_flow(x: 1)                   // calling a flow is a step
  finally {
    // always runs
  }
}
```

### 4.1 Header clauses

- **`in`**: typed input. `= default` makes it optional. `T?` makes it optional with no default; if absent it is unwrapped with `if let`. `secret` marks a credential.
- **`out`**: typed output. Assigned with `set name = expr` anywhere in the flow. Outputs assigned before a failure are still reported ([section 11.3](#113-output-channels)).
- **`requires`**: preconditions, evaluated before any step runs. A failing precondition fails the flow with exit 1 and the expression text in the message. Preconditions must be pure or read-only built-ins such as `exists`.
- **`lock`**: acquire an exclusive advisory lock on the given path for the flow's duration. The path is an inside path and must lie under a `rw` mount, or the lock would exclude nothing ([section 7.1](#71-the-script-defines-the-sandbox)). If it is already held, the flow fails immediately rather than waiting. A held lock is a signal, not a queue, and waiting is how cron jobs pile up.
- **`finally`**: a block that runs whether or not the body succeeded. A failure inside `finally` is reported but does not mask the original failure.

### 4.2 Steps

There is no `step` or `call` keyword. Two forms are steps:

1. A flow call: `fetch(url: u, dest: d)`.
2. An inline named block: `name { ... }`.

Steps have stable identity used by `--plan`, logs, `when`, and `--json` output. Names must be unique within a flow.

**Step modifiers** follow the step:

- **`when <expr>`**: guard. The step is skipped (and reported as skipped) if false.
- **`confirmable`**: a marker that only takes effect when the caller passes `--confirm`. Without `--confirm`, the step is skipped and reported as `skipped: unconfirmed`. The interpreter never prompts. With `--confirm`, the step runs normally.

```weir
reload_nginx {
  systemctl(action: "reload", unit: "nginx")
} confirmable
```

**Alternative rejected: interactive y/n prompts.** A prompt under cron or a systemd unit blocks forever or reads garbage from a closed stdin. Making confirmation a pre-declared flag keeps every run non-interactive and makes it auditable in a crontab line.

**Alternative rejected: `--yes` that auto-answers all prompts.** It implies prompts exist, which brings back the hang problem for anyone who forgets the flag.

### 4.3 Loops and repeated steps

A step inside a loop would otherwise have the same name each iteration. Steps that repeat must carry an explicit `as` label giving each iteration a stable identity; the loop adds the iteration index automatically.

```weir
for host in hosts {
  probe as host {
    // identity: probe[<index>] "<host>"
  }
}
```

The checker errors on a repeated step with no `as` label, since without it the plan, the log, and the JSON output cannot say which iteration failed.

## 5. Types and Values

| Type         | Notes                                                         |
|--------------|---------------------------------------------------------------|
| `string`     | UTF-8 text. Interpolation with `"${expr}"`.                    |
| `int`        | 64-bit signed.                                                |
| `bool`       |                                                               |
| `path`       | Distinct from `string`. `/` joins paths: `dir / "x.log"`.      |
| `duration`   | Literals `30s`, `15m`, `2h`, `21d`. Comparable and addable.   |
| `list<T>`    |                                                               |
| `map<K,V>`   |                                                               |
| `secret`     | Opaque. See below.                                            |
| enums        | Declared inline in an `in`: `in mode: enum(fast, safe)`.      |
| `T?`         | Optional. There is no null. Unwrapped with `if let v = x {}`. |
| records      | Structural: `{host: string, days: int}`.                      |

`path` is distinct from `string` because command-line path inputs are translated through the sandbox's mount table ([section 7.1](#71-the-script-defines-the-sandbox)) and the built-in `delete`/`move` need to know which values are filesystem locations. A `string` is never implicitly converted to a `path`; use `path(text: s)`.

**`secret`.** A `secret` cannot be printed, interpolated into a string, placed in a command's argument list, or written to a file by the checker's rules. It can only reach a program through a filter's environment (`env { NAME = token }`) or its stdin. It is redacted in logs and plan output. It cannot be accepted on argv ([section 11.2](#112-input-flags)).

**Mutability.** `let` bindings are immutable. `var` is explicit and local. There is no mutable module-level state; a file's top level contains declarations only. This removes the class of bugs where a function's behaviour depends on what some other function did to a global.

**No null.** Absence is `T?`. The literal for "no value" is `absent`. The checker requires unwrapping before use, so "forgot to handle the missing case" is a check error, not a runtime crash.

**Alternative rejected: a dynamic "everything is a string" model as in bash.** It is the source of the quoting problem. Weir pays the cost of conversions being explicit (`int(text: s)`).

## 6. Commands and Errors

### 6.1 Argument lists, not strings

A command is invoked with an argument list. There is no shell involved. Quoting and injection bugs cannot be expressed, because no string is ever re-parsed.

```weir
run(cmd: "tar", args: ["-czf", archive, ...$files])
```

`...$list` spreads a list into the argument list. A filter invocation is the same mechanism with the argument template declared once.

**Alternative rejected: allowing `run(cmd: "tar -czf $a $b")`.** Convenient, and exactly the bug generator this language exists to remove. There is no escape hatch to a shell string in v1. If a user needs a shell, `run(cmd: "bash", args: ["-c", script])` is available and visibly marks the boundary; the checker warns on it (`W-SHELL-STRING`).

### 6.2 Failure

A command exiting with a code outside its `ok_codes` (default `[0]`) throws `ExecError`. This is the default for every command, with no `set -e` equivalent to forget.

`try` opts into tolerance and yields an attempt record:

```weir
let attempt = try run(cmd: "grep", args: ["-q", pattern, file])
if attempt.ok { ... }
// attempt.ok: bool, attempt.code: int, attempt.value: T? (stdout, per yields)
```

Errors are typed and caught by type:

```weir
try {
  deploy(version: v)
} catch e: ExecError {
  print(text: "deploy failed: ${e.message}")
  fail message: "deploy failed"
} catch e: CapabilityError {
  fail message: e.message
}
```

Built-in error types: `ExecError`, `CapabilityError`, `IoError`, `ParseError`, `Timeout`. The checker warns on `catch` clauses that cannot match anything the body can throw, so a typo in an error type is not silent.

### 6.3 `fail`

`fail message: "..."` ends the flow with exit code 1. `finally` still runs.

## 7. Capabilities

**What this section guarantees.** A script defines, at the top of its entry file, the filesystem it runs in: a sandbox made of named host folders mounted at chosen paths with a mode each. The interpreter enters that sandbox before it runs any flow. From then on every path in the script, whether it goes to a built-in or to an external program, is a path inside the sandbox, and anything not mounted does not exist. Built-ins and external programs therefore see the same filesystem, and the guarantee needs no per-operation scope checks. "Sandbox" here means the filesystem only: the keyword does not imply any isolation of the network, the clock or other kernel resources. The guarantee covers the filesystem only; network access, and anything reachable through a mount the script granted, are outside it ([section 7.5](#75-not-covered)).

### 7.1 The script defines the sandbox

The entry file (the one named on the `weir run` command line) has at most one file-level `sandbox` block, placed after its `use` lines and before its declarations.

```weir
sandbox {
  in logs: path = "/var/log/nginx"     // host path, can be overridden with --logs
  mount system                         // the read-only system set, at the same paths
  mount logs at "/logs" ro             // host folder `logs`, visible as /logs, read-only
  mount "/etc/nginx" ro                // same path inside and out
  mount "/etc/nginx/conf.d" rw         // a more specific mount overrides a broader one
  mount "/run/lock" rw
}
```

**Mount forms.**

| Form | Meaning |
|------|---------|
| `mount <host> at <inside> ro\|rw` | bind-mount the host folder or file at `<inside>` in the sandbox |
| `mount <path> ro\|rw` | shorthand for `mount <path> at <path> ...`, the same path inside and out |
| `mount system` | the read-only **system set**, each at its own path: `/usr`, `/lib`, `/lib64`, `/bin`, `/sbin`, and the files `/etc/ssl`, `/etc/resolv.conf`, `/etc/hosts`, `/etc/nsswitch.conf`, `/etc/localtime` |

All mounts are `nosuid` and `nodev`. A mount is a folder or a single file. Where mounts overlap, the more specific `<inside>` path wins. `mount system` is never implied.

**Host side and inside side.** `<host>` is a path on the machine running Weir. `<inside>` is the path the script sees. Using different values makes a script portable across hosts: the script refers only to `/data`, and the operator decides which host folder that is. The sandbox block may declare `in` values of type `path` for the host side, like a flow's inputs. They produce flags (`--logs /srv/nginx-logs`) and `--help` entries and are evaluated before the sandbox is entered. They exist only for the sandbox block; flows cannot read them, because after the sandbox is entered the host path is not meaningful to the script.

**What is inside.** The sandbox root is an empty, read-only tmpfs holding the mount points, plus three fixed entries that are not declared: a private writable `/tmp`, a minimal `/dev` (`null`, `zero`, `urandom`, `tty`), and `/proc` for a new PID namespace. The working directory is `/tmp`. The environment is rebuilt rather than inherited: only `PATH`, `LANG`, `TZ`, `HOME` (set to `/tmp`) and what a filter declares in `env`.

**Paths in the script are inside paths.** A `path` literal, a `path` default and every path computed by `/` joining is a path in the sandbox. Built-ins such as `read_text`, `write` and `delete` operate on the sandbox. External programs started by `filter` and `run` run in the same sandbox and inherit its view, so `tail(file: log, n: 10)` passes the program the same path the script used.

**Paths from the command line are host paths.** An operator thinks in host paths, so a `path` input given as `--log /var/log/nginx/access.log` is a host path that Weir translates through the mount table to the inside path. A relative path is resolved against the host working directory first. If the path does not fall under any mount, the run fails with exit 2 and a message that lists the mounts. Defaults written in the script are inside paths. If two mounts expose the same host path, translation is ambiguous and `weir check` reports an error for the mounts it can compare statically; at run time the more specific inside path is used.

**Entering the sandbox.** `weir run` does its work in this order: parse and check the entry file, fetch and verify imported components against the lockfile, read any `--secret-file` and `--secret-stdin` inputs, evaluate the sandbox block and translate command-line paths, then enter the sandbox ([section 7.6](#76-mechanism-and-availability)) and run the flow. Everything that needs host paths happens before the sandbox exists. After that point the interpreter itself cannot see the host outside the mounts.

**Locks.** `lock <path>` in a flow names an inside path. A lock file inside the private root, or inside `/tmp`, would exclude nothing, because each run has its own. The lock path must therefore lie under a `rw` mount (`mount "/run/lock" rw` in the example above), or `weir check` reports `E-SANDBOX-LOCK-UNSHARED`.

**Why the sandbox is declared at the top and not per operation.** One place shows everything the script can touch. Reading the top of the file answers "what can this do to my machine" without reading the flows, and a caller can restrict it further with `--ro` ([section 11.1.1](#1111-run-time-control-flags)).

### 7.1.1 What the sandbox does not cover

- **Anything inside a mount.** A `rw` mount lets built-ins and every program delete everything in it. The sandbox bounds where, not what. `run(cmd: "rm", args: ["-rf", dir])` is confined to the sandbox, but it is not subject to the refusals in [section 7.4](#74-destructive-built-ins). The checker warns when a literal `cmd` or `program` names a well-known destructive tool (`rm`, `mv`, `dd`, `chmod`, `chown`, `mkfs`, `shred`, `truncate`) and suggests the built-in (`W-DESTRUCTIVE-PROGRAM`), because the built-ins have more checks than the sandbox does.
- **Sockets and devices that were mounted.** Mounting the D-Bus socket so that `systemctl` works gives every program that sees it whatever the daemon will do for that caller.
- **The network.** See [section 7.5](#75-not-covered).
- **Flaws in the sandbox itself.** It relies on kernel namespace and mount behaviour. It is a strong boundary against a script's mistakes and an ordinary misbehaving program. It is not a promise against a deliberately hostile program exploiting a kernel bug.

### 7.2 Executable capabilities are inferred

The set of executables a flow may run is computed from its call graph: the `program` of each filter it reaches, and the literal `cmd` of each `run`. A dynamic `cmd` (computed at runtime) is a check error, which is the reason `program` and `cmd` must be literals. This set is an allow-list of which programs may be started. `weir check` also verifies that each program's resolved path lies under a mount in the sandbox, and reports `E-SANDBOX-EXEC-UNREACHABLE` with the fix hint `add mount system, or mount "<dir>" ro` if not. Weir does not add executable or library paths on its own.

**Alternative rejected: requiring an explicit `needs exec` list.** It is redundant with the call graph and goes stale; an inferred set cannot disagree with the code.

### 7.3 Filters narrow the view; components cannot widen it

By default an external program sees the whole sandbox, with every mount in the mode it was declared. A `filter` may narrow what its program sees with `see` lines listing inside paths and a mode:

```weir
filter nginx_test {
  program "nginx"
  args ["-t"]
  see system
  see "/etc/nginx" ro
  see "/var/log/nginx" rw
  takes none
  yields none
}
```

| Form | Meaning |
|------|---------|
| `see <inside> ro\|rw` | the program sees this inside path in this mode |
| `see system` | the system set |

When a filter has `see` lines the program sees only those paths. Each `see` must name a path that is mounted in the script's sandbox, in that mode or a more permissive one: `see "/data" ro` is satisfied by a `rw` mount of `/data`, and `see "/data" rw` is not satisfied by a `ro` mount. Otherwise the call throws `CapabilityError` naming the filter, the path and the mode, and `weir check` reports it when the paths are literal. A filter therefore never sees more than the script's sandbox, and a `see` clause can only remove access, never add it.

**Components.** Only the entry file has a file-level `sandbox` block of `mount` lines. One in an imported file is an error (`E-SANDBOX-NOT-ENTRY`); a scoped `within` block is allowed ([section 7.3.1](#731-scoped-within-blocks)). Filters in imported components use `see` clauses, and those are their requirements: an imported filter that says `see "/etc/ssl" ro` fails the check against a script that did not mount `/etc/ssl`. A library therefore cannot widen what the script exposes, and the requirements show in the component's manifest.

The caller can narrow further with `--ro <inside-path>` (turn a `rw` mount into `ro` for this run) and `--deny-exec <name>`.

**Alternative rejected: a separate sandbox per flow.** It would let one file's flows see different filesystems, but it multiplies the places to read, and a flow calling another flow would need a rule for whose sandbox wins. One file, one sandbox; filters narrow.

**Alternative rejected: no narrowing at all.** Every wrapped program would see every writable folder the script has, so a network client would have the same write access as the backup tool. Narrowing is cheap because `see` is a subset check.

### 7.3.1 Scoped `within` blocks

A filter narrows the view for one program. A `within` block inside a flow narrows it for a stretch of the script, and can hand a value back. Its header lists the view, with the same items as a filter's `see` lines, separated by commas:

```weir
let info = within system, "/etc" ro {
  let kernel = run(cmd: "uname", args: ["-s", "-r"])
  let hosts  = cat_file(file: path(text: "/etc/hosts"))

  return { kernel: first(of: kernel), hosts: count(of: hosts) }
}
print(text: "${info.kernel}: ${info.hosts} lines")
```

The word `sandbox` is used only for the file-level block of [section 7.1](#71-the-script-defines-the-sandbox), which defines the script's whole filesystem. A filter writes `see` lines, and a flow writes `within`. All three describe a view, and only the file-level block can mount anything.

**What it covers.** While the block runs, the script sees only the listed paths, in the listed modes. This applies to every program started inside it, including programs started by flows and filters called from inside it, and to the built-ins that touch the filesystem (`read_text`, `write`, `exists` and the others). A program sees the path as missing. A built-in that reads or writes it throws `CapabilityError` naming the path and the block's line, and `exists` reports `false`, as it does for any path that is not mounted. When a filter inside the block has `see` lines of its own, its program sees the intersection of the two.

**It can only narrow.** Each item must name a path that the enclosing view has, in that mode or a more permissive one, with the same rule as a filter's `see` ([section 7.3](#73-filters-narrow-the-view-components-cannot-widen-it)); otherwise `weir check` reports `E-SEE-UNMOUNTED` when the paths are literal. Blocks nest, and each level can only narrow the one outside it. `--ro` and `--deny-exec` still apply on top. A `within` block in an imported file is allowed, because it cannot widen anything.

**`return`.** `return <expr>` inside the block leaves the block with that value. It never returns from the flow, so it cannot be confused with anything else: a flow sets outputs with `set` and has no `return` of its own.

- A block used as a value (`let x = within ... { ... }`) must return on every path; otherwise `weir check` reports `E-WITHIN-NO-RETURN`. There is no implicit value: the block's last expression is not its result.
- A block used as a plain statement may have no `return`, or a bare `return` that leaves early.
- Every `return` in one block must have the same type.
- A `return` inside a loop inside the block leaves both the loop and the block.
- A thrown error leaves the block like any other scope and nothing is returned.
- Variables declared inside the block are local to it. The value it returns is the way to hand data out.

**Not in a `fn`.** A `fn` is pure and cannot start programs, so a `within` block there is `E-FN-IMPURE`.

**Cost.** Each program started inside the block gets its own narrowed root, built from the block's view just before it starts. On the development machine that adds about 0.6 ms per program (200 `run` calls took 0.18 s inside a block and 0.05 s outside). Building the root once per block and having each program join it is possible later if that ever matters; it would not change the language.

**Alternative rejected: a `see:` argument on `run`.** It repeats the list on every call and cannot cover the built-ins.

**Alternative rejected: the block's last expression as its value.** A trailing `{ ... }` is ambiguous between a record and a nested block, and an implicit result is easy to produce by accident with a stray last line.

**Alternative rejected: reusing `sandbox` for the block.** One word then meant three things: the whole script's filesystem, a filter's narrowing, and a scope with a value. The header of `within` can also grow other clauses later without renaming it.

### 7.4 Destructive built-ins

The built-ins `delete` and `move` refuse the sandbox root `/` and any mount point itself (the mounted folder as a whole, as distinct from its contents), in addition to failing on read-only mounts like any write. This is a backstop for the empty-variable `rm -rf "$DIR/"` case. An external `rm` is confined by the sandbox but is not subject to the extra refusals ([section 7.1.1](#711-what-the-sandbox-does-not-cover)). Because `path` values are never concatenated from strings and an unset input is `absent` rather than empty, most such cases are caught earlier by the type system.

### 7.5 Not covered

Network access is not a declared capability in v1, and the sandbox does not unshare the network namespace, because most useful scripts need the network. A permitted program may open any connection. This is a known gap ([section 19](#19-open-questions-and-known-weaknesses)). Allowed hosts and ports are deferred.

### 7.6 Mechanism and availability

**Mechanism.** The sandbox is a mount namespace entered with `pivot_root`, created through an unprivileged user namespace, so no root is needed. A plain `chroot(2)` is not used: it requires root and is not a security boundary, because a process with root inside a chroot can escape it. Creating a user namespace requires a single-threaded caller, so `weir run` re-executes itself as a small single-threaded stage (`weir __sandbox-run`) that creates the user, mount and PID namespaces, builds the mount table, rebuilds the environment, calls `pivot_root`, and continues as the interpreter inside the sandbox. The interpreter becomes process 1 of the new PID namespace and reaps its children. For a program whose filter has a `sandbox` clause, Weir creates a nested mount namespace for that child and bind-mounts only the `see` paths into it, which a process may do inside its own user namespace.

The interpreter must be able to run after the host is gone. The release binary is statically linked (musl) and does not read host libraries once the sandbox is entered.

**Availability.** v1 supports Linux only, because the mechanism is Linux namespaces. If the sandbox cannot be created (unprivileged user namespaces disabled, a container that forbids them, an unsupported platform), Weir exits 4 with a message naming the cause. It does not fall back to running unconfined unless the operator asks for that with `--sandbox auto` or `WEIR_SANDBOX=auto`.

The only overrides are the CLI option `--sandbox <mode>` and the environment variable `WEIR_SANDBOX`; the option wins over the variable. The modes are:

- `required`, the default: build the sandbox or exit 4.
- `auto`: build it if the host allows, and otherwise warn, naming the cause, and run without it. It is meant for a host that is already isolated, such as a CI job in a container. Weir tests whether the sandbox can be built in a short-lived child process before the script starts.
- `off`: never build it. `--no-sandbox` is the same as `--sandbox off`.

A script cannot choose its own mode. Running without a sandbox prints a warning on stderr and is recorded in the `--json` result as `"sandbox": false`. Without a sandbox no namespaces are created and every inside path is used as a host path, so it works only for a script whose mounts all map a path to itself (`mount "/x" rw`, `mount system`). A script with a remapping mount (`mount logs at "/logs" ro`) exits 2 under `off`, and exits 4 when `auto` fell back because the sandbox could not be built.

A script that uses only built-ins and `fn`s still runs in the sandbox when it declares one; with no `sandbox` block it runs unconfined, and `weir check` reports `W-NO-SANDBOX` for any file that starts an external program without one. Non-Linux hosts can run such scripts.

**Checking the declaration.** Because the script, not Weir, decides what is mounted, a wrong declaration shows up as a path that is not there, a program that cannot start, or a library it cannot find. Weir turns these into messages that name the sandbox:

- `E-SANDBOX-EXEC-UNREACHABLE` (static), as described in [section 7.2](#72-executable-capabilities-are-inferred);
- a filesystem error inside the sandbox (`ENOENT`, `EROFS`, `EACCES`) on a path outside every mount, or on a read-only mount, is reported with the inside path, the nearest mount and its mode;
- if a program fails to start because the loader cannot find a library, the error lists the mount table.

`weir sandbox suggest <file>` is a separate tool that reports, for each filter, the executable and library paths it found by reading the binary's ELF dependencies and `#!` interpreter, and prints suggested `mount` and `see` lines. It is a helper for writing the declaration; nothing is derived from it at run time. It cannot see `dlopen`ed libraries or helper programs the tool runs (`git-*`, an `ssh` `ProxyCommand`).

`weir run --plan` and `weir list --manifest` print the resolved mount table, with host and inside paths, so what a script exposes can be reviewed without running it.

**Costs and open problems.**

- **Verbosity.** Every script that runs an external program carries a sandbox block, and a wrong one is the author's bug. `mount system` removes the worst of the repetition; it does not remove the need to know that `curl` wants `/etc/ssl` or that `systemctl` wants a socket.
- **Tools needing unusual paths.** Programs routinely need more than their obvious paths: locale and certificate stores, NSS modules, helper binaries, daemon sockets. Each is a mount the author must find.
- **Daemon sockets.** `systemctl` and `docker` need their socket mounted `rw`, which punches a hole the sandbox cannot describe. A filter's `see` clause limits which programs get it.
- **Single-file mounts.** A file bind-mounted at a path cannot be replaced by rename, only written in place. Built-ins that normally write a temporary file and rename it must write in place when the target is a mount point.
- **Network confinement** is separate and not included.
- **Bind mounts expose the real tree.** A `rw` mount is writable in full.

**Deferred refinements.** Mounting `rw` paths through overlayfs with a scratch layer would let `weir run --plan` run external programs and report the file changes they made, which would address the biggest weakness of `--plan` ([section 19.5](#195-weaknesses-in-the-design-as-it-stands)), and would allow committing a step's changes only on success. Landlock could serve as a second backend on systems where user namespaces are disabled.

**Alternative rejected: Weir derives the sandbox contents automatically** (system directories by default, executable and library paths found by dependency resolution). It makes scripts shorter, but the real filesystem view then depends on the host's libraries and on resolver logic inside Weir, cannot be read from the script, and fails differently on different machines. An author reading a script should be able to see what it can touch. The discovery work is kept as a suggestion tool instead.

**Alternative rejected: per-operation scope declarations (`needs read` and `needs write`) checked by the interpreter, with externals sandboxed separately.** It gives two path spaces, one for built-ins and one for programs, which have to be kept consistent by the author. A single sandbox that the interpreter itself lives in removes the second space and the scope checks.

**Alternative rejected: leaving confinement to the user (run Weir under `bwrap`, Docker or systemd sandboxing).** The declared filesystem is already in the script, and a sandbox configured separately from it will drift from it. Making the sandbox part of the interpreter keeps the declaration and the enforcement together, and keeps one behaviour on every host that runs the script.

**Alternative rejected: a weaker, best-effort sandbox.** A sandbox that sometimes silently does not apply gives a guarantee that appears to hold and does not. Refusing to run is the only honest alternative to a working sandbox.

## 8. Calls and Naming

Every call uses named arguments, in any order, for flows, filters, fns and built-ins alike. There is no positional form.

```weir
copy(from: a, to: b)
copy(to: b, from: a)     // identical
```

**Rationale.** Models and people mis-order positional arguments, and an unnamed second `path` argument cannot be checked for meaning. With names, the checker can say "missing argument `to`" rather than guess. The cost is verbosity, accepted deliberately.

Optional parameters have defaults. The checker warns (`W-DEFAULT-ARG`) when a call passes an argument equal to the default, because it is noise and often hides a stale assumption when the default later changes. A pragma suppresses the warning where the explicit value is intentional:

```weir
// weir:allow W-DEFAULT-ARG
copy(from: a, to: b, overwrite: false)
```

**Alternative rejected: positional for the first argument.** A single exception to the rule is enough to make the rule unreliable for a model that has to remember which calls are exceptions.

## 9. Pipelines

A pipeline is a chain of stages joined with `.`, in the same method-call style used for chaining in most general-purpose languages. A stage is a native stage, a declared filter, or a `run` command. Weir has no `|` operator. The stage on the left supplies the stage on the right with its input: the output of the previous stage, whether that is a program's stdout, a list, or a stream.

```weir
journalctl(unit: "sshd").filter(where: ...).take(n: 5)
```

A failing stage fails the pipeline, always. There is no `pipefail` switch because there is no mode in which a failure is ignored.

**Parsing rules.** `.name(...)` with parentheses is a stage application; `.name` without parentheses is member access (`it.count`, `left.days`). A line that begins with `.` continues the expression on the previous line, so long chains can be laid out one stage per line.

**Why `.` and not `|`.** Programs are never invoked as bare text in Weir: every one is wrapped in a typed `filter` and called with named arguments. Once that is true, `|` no longer means "connect two processes' file descriptors", and it carries bash's associations (and bash's silent-failure semantics). Dot chaining reads like the rest of the language, works identically on a list that is already in memory and on a stream from a program, and is what most programmers and models will expect, so there is less to learn. The cost is that "a stage" and "a method" look the same; the checker resolves the name against native stages and declared filters, and an unknown name is reported with the list of what was in scope.

**Alternative rejected: keeping `|` as well.** Two spellings for one thing mean every example and every lint rule has to handle both, and a model will mix them.

### 9.1 Native stages

| Stage      | Signature (named args)                    | Streams? |
|------------|-------------------------------------------|----------|
| `lines`    | `lines()`                                 | yes      |
| `map`      | `map(to: expr)`                           | yes      |
| `filter`   | `filter(where: expr)`                     | yes      |
| `take`     | `take(n: int)`                            | yes, and stops upstream early |
| `unique`   | `unique()` or `unique(by: expr)`          | streams output, but holds every distinct key in memory |
| `count`    | `count()`                                 | consumes all input, O(1) memory |
| `count_by` | `count_by(key: expr)`                     | buffers; emits `{key, count}` at end |
| `sort_by`  | `sort_by(key: expr, order: asc\|desc)`    | buffers all input |

Native stages are generic over element type: they work on text lines and on parsed records alike. Inside a stage's expression, `it` is the current element.

```weir
let top = journalctl(unit: "sshd")
  .filter(where: contains(text: it, part: "Failed password"))
  .map(to: ip_of(line: it))
  .count_by(key: it)
  .sort_by(key: it.count, order: desc)
  .take(n: 10)
```

### 9.2 Failure and early termination

When `take` has enough elements it closes its input, which makes the upstream program receive `SIGPIPE`/a write error and exit non-zero. The interpreter must not treat that as a pipeline failure. The rule: a stage terminated by the interpreter's own close is not a failure. A stage that exits non-zero for any other reason is. Distinguishing the two reliably across platforms is implementation work ([section 19](#19-open-questions-and-known-weaknesses)).

### 9.3 Binding

Binding a pipeline to a typed `let` materialises it into the declared type (`list<T>`, `string`, ...). A pipeline in statement position is run for its effect. A pipeline whose last stage is a command with `yields none` is a statement.

**Alternative rejected: lazy streams as a first-class value.** Passing a half-consumed stream around makes lifetime and failure timing hard to reason about. A pipeline either runs where it appears or is materialised where it is bound.

## 10. Components

`use` imports from a local path, a GitHub repository, or an Artifactory/Nexus URL. `#name` selects one flow from a file.

```weir
use "./lib/net.weir"
use "github:acme/weir-lib/rotate.weir@v1.4.0#rotate_files"
use "https://artifactory.example.com/weir/tls.weir@2.1.0#check_expiry"
```

### 10.1 Lockfile

`weir lock` writes `weir.lock`, pinning each remote component by version and content hash (SHA-256). `weir run` and `weir check` fail with exit 3 if a fetched component's hash does not match. Local paths are not pinned.

### 10.2 Manifests

Each component's header (inputs, outputs, defaults, and the `see` requirements of its filters) is parseable without reading its body. `weir list --manifest <file>` prints it, and for an entry file also prints the resolved mount table. This lets a caller check what a component needs, and lets the checker validate calls, without trusting or executing the component. The inferred executable set is part of the manifest and is computed from the component's own call graph.

**Alternative rejected: unpinned `use` of a branch.** A moving dependency makes `weir check` results time-dependent.

## 11. CLI Surface

```text
weir run   <file> [flow] [--flags]
weir check <file> [--json]
weir list  <file> [--manifest]
weir test  [path]
weir lock  [file]
weir sandbox suggest <file>
```

### 11.1 Generated flags

A file-level `sandbox` block's `in` values generate flags in the same way ([section 7.1](#71-the-script-defines-the-sandbox)). Typed inputs generate flags and `--help`. `in count: int = 3` becomes `--count <int>` with `3` shown as default. `list<T>` inputs accept repeated flags. `bool` inputs are `--flag` / `--no-flag`. Enums list their values in `--help`. A value that fails to parse as its declared type is exit 2 with the offending flag named. A `path` value is a host path and is translated to the inside path through the mount table; one that falls under no mount is also exit 2, and the message lists the mounts.

### 11.1.1 Run-time control flags

- `--confirm`: run `confirmable` steps.
- `--plan`: print what would happen and run nothing.
- `--ro INSIDE_PATH`, `--deny-exec NAME`: narrow the sandbox for this run: turn a `rw` mount into `ro`, or refuse to start a program ([section 7.3](#73-filters-narrow-the-view-components-cannot-widen-it)).
- `--sandbox <required|auto|off>`: what to do when the filesystem sandbox cannot be created ([section 7.6](#76-mechanism-and-availability)); `WEIR_SANDBOX` sets the same. `--no-sandbox` is `--sandbox off`. Running without the sandbox prints a warning on stderr and works only for scripts whose mounts all map a path to itself. Not settable from a script.

### 11.2 Input flags

`secret` inputs are never accepted on argv, because argv is visible in `ps` and shell history. They come from:

- the environment variable `WEIR_SECRET_<NAME>`,
- `--secret-file <name>=<path>` (file mode must not be group/world readable),
- `--secret-stdin <name>` (reads stdin to EOF).

Supplying a `secret` as `--token x` is exit 2 with a message that names these alternatives.

### 11.3 Output channels

stdout carries either the script's own output or the machine-readable result, never both.

- By default, `print` writes to stdout, and the result is not emitted.
- With `--json`, stdout carries a single JSON document: `{"ok": bool, "exit": int, "outputs": {...}, "error": {...}?, "steps": [...]}`. `print` is redirected to stderr.
- With `--out <name>`, stdout carries the single named output as raw text (a `list` becomes one element per line). `print` is redirected to stderr.

Diagnostics and logs always go to stderr. The `--json` document includes `"sandbox": true` or, when the script ran without a sandbox, `"sandbox": false`. Outputs assigned before a failure appear in the `--json` document, so a caller can see partial results.

**Alternative rejected: always emit the result to stdout and the human text to stderr.** It makes the common interactive case (`weir run cleanup.weir`) print nothing readable on stdout, and a script that prints and also returns data is the case this rule exists to disambiguate.

## 12. Testing

A `test` declaration runs under `weir test` against fixtures: a temporary directory tree, and stubs for filters.

```weir
test "ip_of takes the first field" {
  expect ip_of(line: "10.0.0.1 - - GET /") == "10.0.0.1"
}

test "ban_noisy_clients writes only offenders" {
  fixture dir "/var/log/nginx" {
    file "access.log" text: "10.0.0.1 ... 500\n10.0.0.2 ... 200\n"
  }
  stub tail yields lines: ["10.0.0.1 ... 500", "10.0.0.2 ... 200"]
  run ban_noisy_clients(threshold: 1)
  expect exists(path: "/etc/nginx/conf.d/blocklist.conf")
}
```

Tests never touch the real filesystem or run real programs unless a test explicitly opts in. Each test runs in its own sandbox made of its fixtures, with each `fixture dir` appearing at its stated path, so a flow under test sees only what the test gave it. `fn` tests need no fixtures. This section is deliberately thin; fixture semantics are listed as an open question.

## 13. Worked Example: Log Analysis and Config Change

Read a web server's access log, find clients producing many error responses, write them to a blocklist included by the server's configuration, validate the configuration, and reload the server, with the reload gated behind `--confirm`.

```weir
// ban_noisy_clients.weir
sandbox {
  mount system
  mount "/var/log/nginx" rw          // the access log; nginx -t also writes its error log here
  mount "/etc/nginx" ro
  mount "/etc/nginx/conf.d" rw       // more specific, so the blocklist and its backup are writable
  mount "/run/lock" rw               // the lock file must be on a shared (host) mount
  mount "/run/systemd" ro
  mount "/run/dbus/system_bus_socket" rw
}

filter tail {
  program "tail"
  in file: path
  in n: int
  args ["-n", string(value: n), file]
  see system                           // narrows: tail sees the system set and the log folder, read-only
  see "/var/log/nginx" ro
  takes none
  yields lines
}

filter nginx_test {
  program "nginx"
  args ["-t"]
  see system
  see "/etc/nginx" ro
  see "/var/log/nginx" rw         // nginx -t opens its error log
  takes none
  yields none
}

filter systemctl {
  program "systemctl"
  in action: enum(reload, restart, status)
  in unit: string
  args [action, unit]
  see system
  see "/run/systemd" ro
  see "/run/dbus/system_bus_socket" rw
  takes none
  yields none
}

fn status_of(line: string) -> int {
  // combined log format: the status code is the 9th space-separated field
  return int(text: nth(of: split(text: line, on: " "), index: 8))
}

fn ip_of(line: string) -> string {
  return nth(of: split(text: line, on: " "), index: 0)
}

fn render_blocklist(ips: list<string>) -> string {
  let rules = ips.map(to: "deny ${it};")
  return join(items: rules, with: "\n") + "\n"
}

main flow ban_noisy_clients {
  in log: path = path(text: "/var/log/nginx/access.log")
  in blocklist: path = path(text: "/etc/nginx/conf.d/blocklist.conf")
  in window: int = 50000        // most recent N log lines to examine
  in threshold: int = 200       // error responses before a client is banned
  out banned: list<string>

  requires exists(path: log)
  lock "/run/lock/ban_noisy_clients.lock"

  var offenders: list<string> = []

  find_offenders {
    let found = tail(file: log, n: window)
      .filter(where: status_of(line: it) >= 400)
      .count_by(key: ip_of(line: it))
      .filter(where: it.count >= threshold)
      .sort_by(key: it.count, order: desc)
      .map(to: it.key)
    offenders = found
    set banned = offenders
  }

  write_blocklist {
    when count(of: offenders) > 0
    let backup = blocklist.with_suffix(text: ".bak")
    copy(from: blocklist, to: backup)
    write(file: blocklist, text: render_blocklist(ips: offenders))
  }

  validate {
    when count(of: offenders) > 0
    nginx_test()
  }

  reload {
    when count(of: offenders) > 0
    systemctl(action: "reload", unit: "nginx")
  } confirmable

  finally {
    print(text: "examined ${window} lines, ${count(of: offenders)} client(s) over threshold")
  }
}
```

Points worth noting:

- The script's whole filesystem is the `sandbox` block. `tail`, `nginx` and `systemctl` each see only their `see` paths, which must be mounted in the sandbox in at least that mode. The backup file lands next to the blocklist, in `/etc/nginx/conf.d`, which is mounted `rw`, so the `copy` succeeds. Had it been written to `/etc/nginx/blocklist.bak`, that path falls under the `ro` mount of `/etc/nginx`, the write fails with a read-only error, and the message names the mount and its mode. Producing that message is the kind of thing the runtime must get right.
- The inferred executable set is `{tail, nginx, systemctl}`. `weir check` confirms each resolves under the mounts the program sees (`mount system` covers `/usr/bin` and `/usr/sbin`). A caller can see the resolved mount table with `weir list --manifest`, and `weir run --plan` prints it. Passing `--log /var/log/nginx/access.log.1` works because that host path falls under the `/var/log/nginx` mount.
- The `systemctl` socket mount is a real hole in the sandbox: it is writable, and `systemd` will act on requests sent through it. The sandbox cannot describe that. The `see` clauses limit it to the one filter that needs it, and the mount is visible at the top of the script.
- The flow is safe to run from cron. Without `--confirm` it updates the blocklist and validates it, then reports `reload: skipped: unconfirmed`. An operator can follow with `weir run ban_noisy_clients.weir --confirm`, or put `--confirm` in the cron line once they trust it.
- If `nginx_test()` fails the flow fails with exit 1 and the `.bak` file is left in place. A production version would restore the backup in a `catch`; it is omitted here to keep the example short.
- `tail(...).filter(...).count_by(...)` streams until `count_by`, which buffers one counter per distinct IP, then `sort_by` buffers the resulting (smaller) set. The log itself is never held in memory.

## 14. Worked Example: Certificate Expiry Checker

Probe a list of hosts, compute days to certificate expiry, classify each, and fail if any is critical or unreachable. It is intended to run from cron and emit JSON for a monitoring system.

```weir
// check_certs.weir
sandbox {
  mount system                       // openssl and date, /etc/ssl, resolver config
}

filter s_client {
  program "openssl"
  in host: string
  in port: int = 443
  args ["s_client", "-connect", "${host}:${port}", "-servername", host]
  takes none
  yields text
  ok_codes [0]
}

filter x509_enddate {
  program "openssl"
  args ["x509", "-noout", "-enddate"]
  takes text
  yields text
  ok_codes [0]
}

filter epoch_of {
  program "date"
  in stamp: string
  args ["-u", "-d", stamp, "+%s"]
  takes none
  yields text
}

fn after_equals(line: string) -> string {
  return nth(of: split(text: line, on: "="), index: 1)
}

fn classify(left: duration, warn: duration, crit: duration) -> string {
  if left <= crit { return "critical" }
  if left <= warn { return "warn" }
  return "ok"
}

main flow check_certs {
  in hosts: list<string>
  in port: int = 443
  in warn_within: duration = 21d
  in fail_within: duration = 7d
  out results: list<{host: string, days: int?, state: string}>

  requires count(of: hosts) > 0

  var rows: list<{host: string, days: int?, state: string}> = []

  for host in hosts {
    probe as host {
      let attempt = try s_client(host: host, port: port).x509_enddate()
      if attempt.ok {
        let stamp = after_equals(line: attempt.value)
        let expiry = int(text: epoch_of(stamp: stamp))
        let left = duration(seconds: expiry - now_epoch())
        let state = classify(left: left, warn: warn_within, crit: fail_within)
        rows = rows + [{host: host, days: left.days, state: state}]
      } else {
        rows = rows + [{host: host, days: absent, state: "unreachable"}]
      }
    }
  }

  set results = rows

  let bad = rows
    .filter(where: it.state == "critical" or it.state == "unreachable")
    .count()
  if bad > 0 {
    fail message: "${bad} certificate(s) critical or unreachable"
  }
}
```

Run forms:

```text
weir run check_certs.weir --hosts a.example.com --hosts b.example.com
weir run check_certs.weir --hosts a.example.com --json
weir run check_certs.weir --hosts a.example.com --out results
```

Points worth noting:

- `try` around the pipeline means an unreachable host does not abort the loop, but the failure is still recorded and still produces a non-zero exit at the end. Tolerance is local and explicit; the final `fail` restores the default.
- With `--json`, a failing run still emits `results`, because `set results = rows` ran before `fail`. A monitoring system gets both the exit code 1 and the full table.
- `probe as host` gives each iteration the identity `probe[i] "<host>"` in the plan, the log, and the `steps` array of the JSON output.
- The sandbox is just `mount system`. The flow touches no files of its own, and its filters have no `see` clause, so `openssl` and `date` see the system set, including `/etc/ssl`, `/etc/resolv.conf` and `/etc/hosts`, and nothing else. Their capability set is `{openssl, date}`. The network reach of `openssl` is not bounded by the sandbox ([section 7.5](#75-not-covered)).
- `classify` is a `fn`, so it is testable without stubs.

## 15. Grammar Sketch

This is a sketch, not a complete grammar. Whitespace is insignificant except as a token separator; newlines terminate statements, and a line ending in `,`, an operator or an open bracket continues. Comments begin with `//`.

```ebnf
file        = { use_decl } [ sandbox_block ] { declaration } ;
use_decl    = "use" string ;

declaration = flow | filter | fn | test ;

flow        = [ "main" | "private" ] "flow" ident "{" { flow_item } "}" ;
flow_item   = in_decl | out_decl | requires | lock
            | finally | statement ;

in_decl     = "in" ident ":" type [ "=" expr ] ;
out_decl    = "out" ident ":" type ;
sandbox_block  = "sandbox" "{" { in_decl | mount } "}" ;     (* entry file only *)
mount       = "mount" ( "system"
                      | expr [ "at" expr ] ( "ro" | "rw" ) ) ;
see         = "see" see_item ;                              (* in a filter, one per line *)
see_item    = "system" | expr ( "ro" | "rw" ) ;
requires    = "requires" expr ;
lock        = "lock" expr ;
finally     = "finally" block ;

filter      = "filter" ident "{" { filter_item } "}" ;
filter_item = "program" string
            | in_decl
            | "args" list_lit
            | see
            | "env" "{" { ident "=" expr } "}"
            | "takes" io_kind
            | "yields" io_kind
            | "ok_codes" list_lit ;
io_kind     = "none" | "lines" | "text" | "json" ;

fn          = "fn" ident "(" [ params ] ")" "->" type block ;
test        = "test" string "{" { test_item } "}" ;

statement   = let | var | assign | set | if | if_let | for | try_stmt
            | return | fail | within_block | step | expr ;
let         = "let" ident [ ":" type ] "=" expr ;
var         = "var" ident [ ":" type ] "=" expr ;
assign      = ident "=" expr ;
set         = "set" ident "=" expr ;
if          = "if" expr block { "else" "if" expr block } [ "else" block ] ;
if_let      = "if" "let" ident "=" expr block [ "else" block ] ;
for         = "for" ident "in" expr block ;
try_stmt    = "try" block { "catch" ident ":" ident block } ;
return      = "return" expr ;
within_block = "within" see_item { "," see_item } "{" { statement } "}" ;   (* a value when bound with let; see 7.3.1 *)
fail        = "fail" "message" ":" expr ;

step        = ( call | ident "{" body "}" ) [ "as" expr ] { modifier } ;
modifier    = "when" expr | "confirmable" ;

expr        = postfix ;
postfix     = primary { "." ident [ "(" [ arg { "," arg } ] ")" ] } ;
                                             (* with parens: a stage (native, filter or run);
                                                without: member access.
                                                A line starting with "." continues the previous one. *)
primary     = call | ident | literal | "(" expr ")" | if_expr | within_block ;
if_expr     = "if" expr block "else" ( block | if_expr ) ;   (* a value: each block ends in an expression *)
call        = [ "try" ] ident "(" [ arg { "," arg } ] ")" ;
arg         = ident ":" expr | "..." "$" ident ;
type        = ident [ "<" type { "," type } ">" ] [ "?" ]
            | "{" ident ":" type { "," ident ":" type } "}" ;
```

Operator precedence, high to low: member access `.`, unary `not` and `-`, `*` `/` (arithmetic and path join), `+` `-`, comparison, `and`, `or`. Chaining with `.` binds tightest, as member access does.

## 16. Reserved Words

```text
absent       and          args         as           at           catch
confirmable  else         enum         env          expect       false
fail         filter       finally      fixture      flow         fn
for          if           in           it           json         let
lines        lock         main         mount        none         not
ok_codes     or           out          private      program      requires
return       ro           run          rw           sandbox      secret
see          set          stub         system       takes        test
text         true         try          use          var          when
within       yields
```

`it` is reserved in pipeline stage expressions only; elsewhere the checker rejects its use as a binding name to avoid ambiguity. Names in the error-type list (`ExecError`, `CapabilityError`, `IoError`, `ParseError`, `Timeout`) are reserved in type position.

## 17. Comparison to Other Tools

| Tool           | What Weir takes                                                          | Where Weir differs |
|----------------|--------------------------------------------------------------------------|--------------------|
| **Nushell**    | Structured data through pipelines; commands as typed stages.             | Nushell is an interactive shell with a large command set. Weir has no shell mode, a small fixed set of native stages, and requires declared wrappers around external programs. |
| **YSH / Oil**  | Explicit argument lists instead of re-parsed strings; errors that stop the script by default. | YSH is a gradual migration path from bash and keeps the bash surface alongside the new one. Weir has no bash surface to fall back on. |
| **PowerShell** | Typed parameters, generated help, objects through pipelines.             | PowerShell is large, ambient-authority, and verb-noun heavy. Weir adds capability declarations and a purity-checked `fn`. |
| **Amber**      | A language designed to be safer than bash, with failure handling forced on the author. | Amber compiles to bash. Weir interprets directly, so there is no generated script to debug and no bash quoting underneath. |
| **GitHub Actions** | Typed inputs, named steps, declared outputs, reusable components with versions. | Actions is YAML plus shell strings, with expression syntax embedded in strings and no static check of step bodies. Weir has an actual grammar, checks bodies, and is not tied to a CI runner. |

None of these is rejected as wrong. Weir is aimed at a narrower target: scripts where the cost of a mistake is high, where the author may be a model, and where the same file must run unattended.

## 18. Implementation Notes

The reference implementation is intended to be written in Rust. File extension: `.weir`.

- **Single static binary.** Rust produces one self-contained executable that can be copied to a host that has none of the toolchain, which matters for a tool that replaces bash on arbitrary hosts.
- **Hand-written recursive-descent parser.** Parser generators give weak error messages and weak recovery. Since error quality is the product ([section 19](#19-open-questions-and-known-weaknesses)), the parser is written by hand to control the spans, the expected-token lists, and recovery so that one mistake does not produce a cascade.
- **Direct process spawning.** Commands are spawned with an argument vector and no shell. On Linux the interpreter enters the sandbox once, at startup of a run, by re-executing itself as a single-threaded stage (`weir __sandbox-run`) that creates the user, mount and PID namespaces, builds the declared mount table (plus the fixed `/tmp`, `/dev` and `/proc`), rebuilds the environment, calls `pivot_root`, and continues as the interpreter. The stage is needed because `unshare(CLONE_NEWUSER)` requires a single-threaded process. The release binary is statically linked (musl) so it needs nothing from the host once inside. As PID 1 of the new PID namespace the interpreter must reap orphaned children. Filters with a `see` clause start their program in a nested mount namespace. Namespace and mount calls go through `libc` or `nix`. Dependency discovery for `weir sandbox suggest` reads the ELF headers directly rather than invoking `ldd`, which executes the binary.
- **Alternative considered: Go.** Equally capable of a static binary and quicker to write. Rust is preferred for the sum types and exhaustive matching that the checker's data model (types, capabilities, findings) benefits from. This is a preference, not a hard constraint, and the choice does not affect the language.

## 19. Open Questions and Known Weaknesses

### 19.1 No training data, so error messages are the product

Weir does not exist, so no model has seen it. A model asked to write Weir will write something that looks like Rust, Python or bash, and get it wrong in ways particular to Weir. The language cannot rely on familiarity. It can only rely on the check pass saying exactly what is wrong and how to fix it, in a form a model can act on in one step (`fix` hints, stable codes, JSON output).

This means error-message quality is a feature to be tested and measured, not polished at the end. A concrete proposal: before freezing the grammar, collect a set of plausible incorrect scripts written by models and people, run `weir check` on them, and revise the grammar or the messages wherever the fix hint does not lead to a correct script on the next attempt. Whether this works is unproven.

The named-argument rule helps here (there are fewer ways to be wrong) but does not solve it.

### 19.2 Language surface versus "simpler than bash"

The goal is a language simpler than bash for the target scripts. Counting the concepts in this document: four declarations, nine header or step modifiers, eleven types and forms, structured errors, a mandatory filesystem sandbox that the entry file defines mount by mount, with `see` narrowing in filters and an escape flag, a lockfile, a fixture system, and a native-stage vocabulary. That is not obviously smaller than bash plus a linter, and a reader could reasonably call it larger. Bash is bad, but it is small.

The bet is that these concepts are each individually easy and rarely interact, and that the checker removes the need to learn the sharp edges. That is unverified. Candidates for removal if the surface proves too large: `lock` (a built-in could do it), `requires` (an `if ... fail` does the same), records as a distinct type (use maps), the `finally` block.

### 19.3 Things added beyond the original brief

- **Structural record types** appear in the type table. They are needed for `count_by` output and parsed JSON, but they make Weir's type system larger than the "distinct types" list in the brief. An alternative is `map<string, T>` throughout, at the cost of heterogeneous rows.
- **`set` for outputs and `try` as an attempt record** are syntactic choices made here without much competition from alternatives.
- **The filesystem sandbox** ([section 7](#7-capabilities)) with a file-level `sandbox` block, host-to-inside remapping, and `see` clauses on filters are not in the original brief. They replace the brief's `needs read` and `needs write`, which Weir could only have enforced on its own built-ins. The sandbox also changes how `path` inputs behave on the command line.
- **Scoped `within` blocks** ([section 7.3.1](#731-scoped-within-blocks)) narrow the view for part of a flow and use `return` for a value. They are an addition after the first interpreter, not part of the original design.
- **`$` in `...$list`** appears only in spread. Variables are otherwise bare. It is inconsistent and should probably be `...list`.

### 19.4 Deferred past v1

- Parallelism (concurrent steps or `for` iterations).
- Network capabilities (allowed hosts, ports).
- Sandbox refinements ([section 7.6](#76-mechanism-and-availability)): overlayfs-backed `--plan` and commit-on-success, a Landlock backend where user namespaces are disabled, network namespaces.
- Sandbox support outside Linux (macOS, BSDs). v1 refuses to run a script that starts external programs where it cannot sandbox them, unless `--no-sandbox` is given.
- Windows support. The v1 design assumes POSIX semantics for signals, locks, and paths.
- A package index. Components come from paths, GitHub and Artifactory/Nexus only.
- Interactive use and a REPL.
- Declared timeouts on steps and commands (only `Timeout` as an error type is reserved).
- Resumable runs (re-running only failed steps).
- An editor language server, beyond the JSON output of `weir check`.
- Formal denial of `run(cmd: "bash", ...)`. It is only warned about in v1.

### 19.5 Weaknesses in the design as it stands

- **`--plan` cannot be complete.** It does not execute commands, so any control flow depending on a command's output cannot be resolved. The plan shows both branches annotated with the condition, which can overstate what will happen. A plan is a conservative superset of effects, not a prediction. Whether this is useful enough in practice is open.
- **Dot chaining blurs stage and method.** `.filter(...)` could be a native stage, a declared filter of the same name, or a future method on a type. Name resolution order must be fixed (declared filters shadowing natives is probably wrong) and the error for an unknown name must list candidates.
- **Pipeline early termination.** Distinguishing "closed by `take`" from a real failure across platforms and programs is delicate. Some programs report a broken pipe with a distinct code, some with a message, some hang. This may need a per-filter declaration.
- **`confirmable` semantics for dependent steps.** If a later step depends on the skipped step's effect, running it is wrong. The current rule only skips the one step. A `needs_step` dependency or a rule that skips all later steps in the flow is needed.
- **`fn` purity versus reading the clock.** `now_epoch()` is not allowed in a `fn`, so time-dependent logic must be passed in as an argument. This is correct and mildly annoying.
- **Secrets in filters.** `env { NAME = token }` is the only route for a secret to a program besides stdin. Some programs accept credentials only on argv. Those cannot be wrapped safely, and the design offers no answer beyond "write a wrapper".
- **Network is outside the capability model.**
- **The sandbox is Linux-only and may be unavailable.** It depends on unprivileged user namespaces, which are disabled on some hardened hosts and forbidden in some containers. There the run exits 4 unless the operator chooses `--sandbox auto` or `off`, which work only for scripts without remapping mounts. macOS and the BSDs have no v1 sandbox, so any script that runs an external program is Linux-only in practice. For a language meant to replace bash everywhere, this is a significant limit. Whether to accept it, or to add a weaker non-Linux backend, is open.
- **Command-line paths are translated, not taken literally.** An operator passes host paths and the script sees inside paths, so an error message that mentions a path must say which kind. A path that is not under any mount fails with exit 2 before the script starts. Translating paths in `list<path>` inputs and in paths embedded in other values (a URL, a config file) is not possible; a program given a host path inside a string will not find it.
- **The interpreter lives inside the sandbox it enforces.** A bug that lets a script call a built-in on a path outside the mount table is not possible by construction, but a bug in the pre-sandbox stage (path translation, lockfile verification, secret reading) happens outside it and has host access. That stage is the part of the interpreter that must be small and audited.
- **Script-defined mounts are verbose and easy to get wrong.** Programs need paths nobody thinks of: certificate stores, locale data, NSS modules, helper binaries, daemon sockets. Because the author lists the mounts, a missing one is the author's bug and shows up as a program failing to start or to find a library. `mount system`, the `E-SANDBOX-EXEC-UNREACHABLE` check and `weir sandbox suggest` are the mitigations. If authors find this too burdensome they will either mount broadly (a flow cannot mount `/` read-write, but `/usr`, `/etc` and `/var` are not so limited) or run with `--no-sandbox`, and the guarantee shrinks to nothing. Whether explicit mounts or automatic derivation yields better real-world sandboxs is open; the design chose explicit and records the alternative in section 7.6.
- **Fixtures are under-specified.** Stubbing at the filter level does not test the real program's behaviour, and the fixture language for directory trees is only sketched.
- **Naming strictness has a cost.** Named-only calls make short scripts noticeably longer than their bash equivalents. This may hurt adoption for one-liners, which are a large share of what bash is used for. A `weir -e` one-liner mode is not designed.

### 19.6 Checker rules that are expensive to implement

Roughly in order of cost:

1. **Explaining sandbox failures.** The kernel reports `ENOENT`, `EROFS` or `EACCES`. The message must say which mount a path fell under (or that it fell under none), in terms of inside paths, with the host path alongside when it differs. Doing this for built-ins and for errors printed by external programs, which Weir can only see as text, is uneven. There is no static scope proof to implement: unlike per-operation scope checks, the sandbox needs no symbolic path reasoning, because anything outside a mount does not exist. The checker's sandbox rules are limited to `E-SANDBOX-EXEC-UNREACHABLE`, `E-SANDBOX-LOCK-UNSHARED`, `E-SANDBOX-NOT-ENTRY` and literal `see` checks.
2. **Capability inference through the call graph** with imports from remote components. It requires computing the manifest of each component without executing it, and it must handle recursion and diamond imports.
3. **Pipeline element-type inference** through generic stages with `it`. Requires bidirectional inference and good error messages when it fails, which is hard because the error appears far from the cause.
4. **`secret` flow analysis.** Tracking that no expression derived from a `secret` reaches an argument list, an interpolation, a file write or an output. Has to follow values through `fn` returns and record fields.
5. **`W-DEFAULT-ARG` with constant folding.** Detecting that an argument equals the default needs evaluation of constant expressions, including durations and path joins.
6. **Exhaustiveness of `catch` and reachability of thrown types.** Needs each built-in and each filter to declare which errors it may throw.
7. **Dependency discovery for `weir sandbox suggest`.** Reading ELF headers for `DT_NEEDED` entries, following `RPATH`/`RUNPATH` and the loader search path, resolving `#!` chains, and doing so identically to what the dynamic loader will do at run time. Anything loaded with `dlopen` is invisible and has to be found by the author. It is not on the run-time path, so a mistake costs a bad suggestion, not a wrong sandbox. The `E-SANDBOX-EXEC-UNREACHABLE` check is a smaller version of the same problem: resolving `program` through `PATH` and symlinks the way the runtime will.
8. **Purity checking of `fn`.** Cheap on its own, but must be re-run through every imported `fn` and built-in, so every built-in needs a purity annotation that stays correct.

### 19.7 Questions not yet decided

- Should `use` allow importing a `fn` or `filter` on its own, or only flows? The manifest model currently assumes flows, but filters are the most reusable thing.
- How are exit codes from nested flow calls reflected in `--json` `steps`? One failure should not produce a confusing cascade of "failed" entries.
- Should a sandbox block be allowed to `include` a shared set of mounts from a component (a `mount web_server` preset), or is `mount system` the only built-in shorthand? Presets would cut repetition and add a fifth thing to define and import.
- Should `ok_codes` allow named meanings (for example, `grep` code 1 meaning "no match" and so `yields` an empty result), rather than only a list of numbers?
