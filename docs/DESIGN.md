# Weir: Design Document

**Status: early design.** Nothing described here is implemented. The language, its syntax and its tooling are all subject to change, and several sections end in open questions. This document is the specification that would precede an implementation.

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
7. [Capabilities and the Filesystem Jail](#7-capabilities)
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
5. **Effects are declared, and enforced.** What a script may read and write, and which programs it may start, is visible from its header, and a caller can bound it. Built-ins are checked by the interpreter and external programs run in a filesystem jail built from the same declaration. The network is not covered.
6. **Error messages are the product.** See [section 19](#19-open-questions-and-known-weaknesses).

## 2. Execution Model

Weir is a single interpreter binary. `weir run file.weir` parses and interprets the file directly. There is no ahead-of-time compile step and no generated code in any other language. This is a deliberate rejection of the "transpile to bash" approach: a transpiled script produces errors that refer to generated lines the author never wrote, and it inherits bash's quoting hazards at the output layer.

### 2.1 Subcommands that matter for the model

**`weir check <file>`** is a static pass. It parses, resolves names, type-checks, infers capabilities, and runs lint rules. It never runs a command or reads the filesystem beyond the source files and the lockfile. Each finding has:

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
| 4    | capability denied (an operation outside the granted capabilities, or a jail that cannot be created) |

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
  needs read  "/etc/example"
  needs write "/var/lib/example"
  requires exists(path: "/etc/example")
  lock "/run/lock/example.lock"

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
- **`needs read` / `needs write`**: filesystem paths or path expressions the flow may touch ([section 7](#7-capabilities)).
- **`requires`**: preconditions, evaluated before any step runs. A failing precondition fails the flow with exit 1 and the expression text in the message. Preconditions must be pure or read-only built-ins such as `exists`.
- **`lock`**: acquire an exclusive advisory lock on the given path for the flow's duration. If it is already held, the flow fails immediately rather than waiting. A held lock is a signal, not a queue, and waiting is how cron jobs pile up.
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

`path` is distinct from `string` because the capability checks ([section 7](#7-capabilities)) and the built-in `delete`/`move` need to know which values are filesystem locations. A `string` is never implicitly converted to a `path`; use `path(text: s)`.

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

**What this section guarantees.** A flow declares the filesystem it may read and write, and Weir enforces that declaration on everything the flow does, including external programs. Built-ins are checked by the interpreter. External programs run inside a filesystem jail ([section 7.6](#76-filesystem-jail)) whose contents the script declares itself: the folders to import and the mode of each. The jail is a v1 requirement, not an option. The guarantee covers the filesystem only; network access, and anything reachable through a path the flow was granted, are outside it ([section 7.5](#75-not-covered)).

### 7.1 Filesystem scope is declared and evaluated at runtime

A flow declares what it may read and write with `needs read` and `needs write`. Paths may use inputs: `needs write dir`.

Enforcement uses the scope after every string and path expression has been evaluated to a concrete, normalised path (`..` resolved, symlinks resolved against the real filesystem). Because paths are computed from inputs, string operations and `/` joins, nothing short of evaluation can say where an operation will land. The evaluated scope is used in two places:

- a **built-in** that touches a path outside it throws `CapabilityError` (exit 4);
- an **external program** is started in a jail containing the flow's `needs` paths plus the mounts that the script's `jail` clauses declare. A path in neither does not exist for the program.

`weir check` also reports a violation when it can prove one without evaluating anything (a literal path outside a literal scope). That is a convenience for earlier feedback. A flow that passes `weir check` can still raise `CapabilityError` at runtime, and the checker never claims otherwise.

### 7.1.1 What the declared scope does not cover

- **Anything inside a granted path.** `needs write dir` lets an external program delete everything in `dir`. The jail bounds where, not what. `run(cmd: "rm", args: ["-rf", dir])` is therefore confined to `dir`, but it is not protected by the refusals in [section 7.4](#74-destructive-built-ins). The checker still warns when a literal `cmd` or `program` names a well-known destructive tool (`rm`, `mv`, `dd`, `chmod`, `chown`, `mkfs`, `shred`, `truncate`) and suggests the built-in (`W-DESTRUCTIVE-PROGRAM`), because the built-ins have more checks than the jail does.
- **Sockets and devices the flow was granted.** A filter that needs the D-Bus socket to run `systemctl` receives write access to that socket, and through it whatever the daemon will do for that caller.
- **The network.** See [section 7.5](#75-not-covered).
- **Flaws in the jail itself.** It relies on kernel namespace and mount behaviour. It is a strong boundary against a script's mistakes and an ordinary misbehaving program. It is not a promise against a deliberately hostile program exploiting a kernel bug.

### 7.2 Executable capabilities are inferred

The set of executables a flow may run is computed from its call graph: the `program` of each filter it reaches, and the literal `cmd` of each `run`. A dynamic `cmd` (computed at runtime) is a check error, which is the reason `program` and `cmd` must be literals. This set is an allow-list of which programs may be started, and `weir check` verifies that each program's resolved path is reachable through the mounts declared for it ([section 7.6](#76-filesystem-jail)). Weir does not add executable or library paths on its own.

**Alternative rejected: requiring an explicit `needs exec` list.** It is redundant with the call graph and goes stale; an inferred set cannot disagree with the code.

**Alternative considered: inferring filesystem capabilities too.** Rejected: paths are frequently computed from inputs, so inference would produce either imprecise (everything) or unsound results, and inference could never see what an external program does. Declaring them is also documentation for the reader and the caller.

### 7.3 Intersection and filter mounts

A called component receives the intersection of its own declaration and the caller's grant. A library flow that declares `needs write "/"` cannot exceed what its caller allowed, so importing a component cannot widen the script's reach, either through built-ins or through the jail it starts programs in. The caller's grant is the caller's declaration intersected with anything imposed from the CLI (`--allow-write PATH`, `--deny-exec NAME`).

A `filter` declares the extra folders its program needs with a `jail` clause ([section 7.6](#76-filesystem-jail)): a configuration directory, a daemon socket, a PID file, the system directories. Mounts outside the system set are **requirements, not grants**. They must lie within the grant of the flow that calls the filter, or the call throws `CapabilityError` naming the filter and the path. This keeps what a program can touch visible in the calling flow's header, and in the manifest, rather than hidden in a wrapper, and stops a library from widening the script's reach.

### 7.4 Destructive built-ins

The built-ins `delete` and `move` refuse any path outside the declared write scope, and unconditionally refuse `/` and the user's `$HOME`, even if the scope would otherwise include them. This is a backstop for the empty-variable `rm -rf "$DIR/"` case. An external `rm` is confined by the jail to the write scope but is not subject to the extra refusals ([section 7.1.1](#711-what-the-declared-scope-does-not-cover)). Because `path` values are never concatenated from strings and an unset input is `absent` rather than empty, most such cases are caught earlier by the type system.

### 7.5 Not covered

Network access is not a declared capability in v1, and the jail does not unshare the network namespace, because most useful scripts need the network. A permitted program may open any connection. This is a known gap ([section 19](#19-open-questions-and-known-weaknesses)). Allowed hosts and ports are deferred.

### 7.6 Filesystem jail

Every external program started by a `filter` or `run` runs in a jail. The script defines the jail's filesystem: it names the folders to import and the mode of each. Weir adds nothing the script did not name, apart from the small fixed infrastructure listed below. So "the program can only touch what the script declared" is true by construction, and the whole view can be read from the source.

**Declaring mounts.** A `jail` clause lists mounts. Each mount imports a host folder (or file) into the jail at the same path, in one of two modes:

```weir
filter nginx_test {
  program "nginx"
  args ["-t"]
  jail {
    mount system                   // the fixed read-only system set, see below
    mount "/etc/nginx" ro
    mount "/var/log/nginx" rw
  }
  takes none
  yields none
}
```

| Form | Meaning |
|------|---------|
| `mount <path> ro` | bind-mount read-only, `nosuid`, `nodev` |
| `mount <path> rw` | bind-mount read-write, `nosuid`, `nodev` |
| `mount system` | shorthand for the read-only **system set**: `/usr`, `/lib`, `/lib64`, `/bin`, `/sbin`, and the files `/etc/ssl`, `/etc/resolv.conf`, `/etc/hosts`, `/etc/nsswitch.conf`, `/etc/localtime` |

`mount system` is never implied. A filter with no `jail` clause has an empty filesystem and, unless the executable is reachable some other way, cannot start (see "Checking the declaration" below). The shorthand exists because nearly every filter would otherwise repeat the same five lines.

Mount paths may use inputs: `mount dir rw`. They are evaluated to concrete, normalised paths before the program starts, the same as `needs` paths.

**Where `jail` may appear.** In a `filter`, for the program that filter wraps. In a `flow`, for bare `run` commands in that flow's body, which have no filter of their own. A flow's `needs read` and `needs write` paths are also imported into the jail of every external program the flow starts, at the stated mode (`read` as `ro`, `write` as `rw`), so a path the flow's own built-ins can use is also visible to the programs it runs. A path given only in a `jail` clause is visible to the program and not to the flow's built-ins.

**Limits on what a component may mount.** A filter from an imported component can name mounts, so mounts need the same intersection rule as `needs` ([section 7.3](#73-intersection-and-filter-mounts)):

- `mount system` and the paths in the system set may always be mounted `ro`;
- any other mount, in either mode, must lie within the calling flow's grant (`needs read` for `ro`, `needs write` for `rw`; a `needs write` path also satisfies a `ro` mount);
- otherwise the call throws `CapabilityError` naming the filter, the path and the mode, and `weir check` reports it when the paths are literal.

So an imported filter cannot quietly mount `/home` or write outside what the calling flow granted, and cannot mount `/` read-write.

**Fixed infrastructure.** Three things are present in every jail and are not declared: a private `/tmp`, a minimal `/dev` (`null`, `zero`, `urandom`, `tty`), and a private `/proc` inside a new PID namespace, so the program cannot see or signal other processes. The environment is rebuilt rather than inherited: only `PATH`, `LANG`, `TZ`, `HOME` (pointing into the private `/tmp`) and the variables the filter declares in `env`.

**Checking the declaration.** Because the script, not Weir, decides what is mounted, a wrong declaration shows up as a program that cannot start or cannot find a library. `weir check` and the runtime catch the common cases:

- `weir check` resolves each filter's `program` against the mounts it declares and reports `E-JAIL-EXEC-UNREACHABLE` if the executable's resolved path lies under none of them, with the fix hint `add mount system, or mount "<dir>" ro`.
- At run time, if the program fails to start because the loader cannot find a library, the error names the jail and lists the mounts, instead of passing through a bare `exec` failure.
- `weir jail suggest <file>` is a separate tool that reports, for each filter, the executable and library paths it found by reading the binary's ELF dependencies and `#!` interpreter, and prints a suggested `jail` clause. It is a helper for writing the declaration. Nothing is derived from it at run time. It cannot see `dlopen`ed libraries or helper programs the tool runs (`git-*`, an `ssh` `ProxyCommand`), which is why the author stays in control.

**Mechanism.** The jail is a mount namespace entered with `pivot_root`, created through an unprivileged user namespace, so no root is needed. A plain `chroot(2)` is not used: it requires root and is not a security boundary, because a process with root inside a chroot can escape it. Creating a user namespace requires a single-threaded caller, and the interpreter is multithreaded, so the interpreter re-executes itself as a small helper (`weir __jail-exec`) that sets up the namespaces and mounts, rebuilds the environment, then `exec`s the target program.

**Availability.** v1 supports Linux only, because the mechanism is Linux namespaces. If the jail cannot be created (unprivileged user namespaces disabled, a container that forbids them, an unsupported platform), Weir refuses to start the program, throws `CapabilityError`, and exits 4 with a message naming the cause. It does not fall back to running unconfined. The only override is the CLI flag `--no-jail`, which cannot be set from a script, is printed in a warning on stderr, and is recorded in the `--json` result as `"jail": false`. A script that only uses built-ins and `fn`s needs no jail and runs anywhere the interpreter does.

`weir run --plan` and `weir list --manifest` print each external program's resolved mount table, so what a script exposes can be reviewed without running it.

**Costs and open problems.**

- **Verbosity.** Every filter carries a `jail` clause, and a wrong one is the author's bug. This is the price of declaring the filesystem explicitly. `mount system` removes the worst of the repetition; it does not remove the need to know that `curl` wants `/etc/ssl` or that `systemctl` wants a socket.
- **Tools needing unusual paths.** Programs routinely need more than their obvious paths: locale and certificate stores, NSS modules, helper binaries, daemon sockets. Each is a mount the author must find. `weir jail suggest` finds only what static analysis can.
- **Daemon sockets.** `systemctl` and `docker` need their socket mounted `rw`, which punches a hole the filesystem scope cannot describe. The mount must lie within the calling flow's `needs write` ([section 7.3](#73-intersection-and-filter-mounts)), so the grant is visible in the flow header.
- **No path remapping.** A mount appears at the same path inside the jail as outside. Mounting a host folder at a different path (`mount "/srv/work" at "/work" rw`) is not in v1.
- **Network confinement** is separate and not included.
- **Bind mounts expose the real tree.** A `rw` mount is writable by the program in full.

**Deferred refinements.** Mounting `rw` paths through overlayfs with a scratch layer would let `weir run --plan` run external programs and report the file changes they made, which would address the biggest weakness of `--plan` ([section 19.5](#195-weaknesses-in-the-design-as-it-stands)), and would allow committing a step's changes only on success. Landlock could serve as a second backend on systems where user namespaces are disabled. Neither is in v1.

**Alternative rejected: Weir derives the jail contents automatically** (system directories by default, executable and library paths found by dependency resolution). It makes scripts shorter, but the real filesystem view then depends on the host's libraries and on resolver logic inside Weir, cannot be read from the script, and fails differently on different machines. An author reading a script should be able to see what its programs can touch. The discovery work is kept as a suggestion tool instead.

**Alternative rejected: leaving confinement to the user (run Weir under `bwrap`, Docker or systemd sandboxing).** The declared scope is already in the script, and a jail configured separately from it will drift from it. Making the jail part of the interpreter keeps the declaration and the enforcement together, and keeps one behaviour on every host that runs the script.

**Alternative rejected: a weaker, best-effort jail.** A jail that sometimes silently does not apply gives a guarantee that appears to hold and does not. Refusing to run is the only honest alternative to a working jail.

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

Each component's header (inputs, outputs, defaults, capabilities) is parseable without reading its body. `weir list --manifest <file>` prints it. This lets a caller check what a component needs, and lets the checker validate calls, without trusting or executing the component. The inferred executable set is part of the manifest and is computed from the component's own call graph.

**Alternative rejected: unpinned `use` of a branch.** A moving dependency makes `weir check` results time-dependent.

## 11. CLI Surface

```text
weir run   <file> [flow] [--flags]
weir check <file> [--json]
weir list  <file> [--manifest]
weir test  [path]
weir lock  [file]
weir jail suggest <file>
```

### 11.1 Generated flags

Typed inputs generate flags and `--help`. `in count: int = 3` becomes `--count <int>` with `3` shown as default. `list<T>` inputs accept repeated flags. `bool` inputs are `--flag` / `--no-flag`. Enums list their values in `--help`. A value that fails to parse as its declared type is exit 2 with the offending flag named.

### 11.1.1 Run-time control flags

- `--confirm`: run `confirmable` steps.
- `--plan`: print what would happen and run nothing.
- `--allow-write PATH`, `--deny-exec NAME`: narrow the grant ([section 7.3](#73-intersection-and-filter-mounts)).
- `--no-jail`: start external programs without the filesystem jail ([section 7.6](#76-filesystem-jail)). Prints a warning on stderr. Exists for hosts where jails cannot be created; not settable from a script.

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

Diagnostics and logs always go to stderr. The `--json` document includes `"jail": true` or, when `--no-jail` was passed, `"jail": false`. Outputs assigned before a failure appear in the `--json` document, so a caller can see partial results.

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

Tests never touch the real filesystem or run real programs unless a test explicitly opts in. `fn` tests need no fixtures. This section is deliberately thin; fixture semantics are listed as an open question.

## 13. Worked Example: Log Analysis and Config Change

Read a web server's access log, find clients producing many error responses, write them to a blocklist included by the server's configuration, validate the configuration, and reload the server, with the reload gated behind `--confirm`.

```weir
// ban_noisy_clients.weir
filter tail {
  program "tail"
  in file: path
  in n: int
  args ["-n", string(value: n), file]
  jail { mount system }             // the file argument is visible through the flow's needs read
  takes none
  yields lines
}

filter nginx_test {
  program "nginx"
  args ["-t"]
  jail {
    mount system
    mount "/etc/nginx" ro           // outside the system set, so the calling flow must grant it
    mount "/var/log/nginx" rw       // nginx -t opens its error log
  }
  takes none
  yields none
}

filter systemctl {
  program "systemctl"
  in action: enum(reload, restart, status)
  in unit: string
  args [action, unit]
  jail {
    mount system
    mount "/run/systemd" ro
    mount "/run/dbus/system_bus_socket" rw
  }
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

  needs read log
  needs write blocklist
  needs read  "/etc/nginx"          // the filters below mount these, so the flow must grant them
  needs write "/var/log/nginx"
  needs read  "/run/systemd"
  needs write "/run/dbus/system_bus_socket"
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

- `needs write blocklist` bounds the built-ins `write` and `copy`, and `blocklist` is also imported read-write into the jail of every program the flow starts. `nginx` and `systemctl` run with exactly the mounts their `jail` clauses name, plus the flow's `needs` paths; nothing else exists for them. The backup file lands next to the blocklist, so it is within the declared scope only if the declaration covers the parent directory. As written, `blocklist` is an input, so the checker cannot prove the violation. At runtime the interpreter evaluates `backup`, finds it outside `needs write blocklist`, and the `copy` throws `CapabilityError` (exit 4) with a message suggesting `needs write blocklist.parent()`. This is a deliberate sample of the kind of error the runtime must get right.
- The inferred executable set is `{tail, nginx, systemctl}`. `weir check` confirms each resolves under the mounts of its own filter (`mount system` covers `/usr/bin` and `/usr/sbin`). A caller can see the mount tables with `weir list --manifest`, and `weir run --plan` prints them.
- The `systemctl` socket mount is a real hole in the jail: it is writable, and `systemd` will act on requests sent through it. The filesystem scope cannot describe that, and the example makes the mount visible in both the filter and the flow header rather than hiding it.
- The flow is safe to run from cron. Without `--confirm` it updates the blocklist and validates it, then reports `reload: skipped: unconfirmed`. An operator can follow with `weir run ban_noisy_clients.weir --confirm`, or put `--confirm` in the cron line once they trust it.
- If `nginx_test()` fails the flow fails with exit 1 and the `.bak` file is left in place. A production version would restore the backup in a `catch`; it is omitted here to keep the example short.
- `tail(...).filter(...).count_by(...)` streams until `count_by`, which buffers one counter per distinct IP, then `sort_by` buffers the resulting (smaller) set. The log itself is never held in memory.

## 14. Worked Example: Certificate Expiry Checker

Probe a list of hosts, compute days to certificate expiry, classify each, and fail if any is critical or unreachable. It is intended to run from cron and emit JSON for a monitoring system.

```weir
// check_certs.weir
filter s_client {
  program "openssl"
  in host: string
  in port: int = 443
  args ["s_client", "-connect", "${host}:${port}", "-servername", host]
  jail { mount system }
  takes none
  yields text
  ok_codes [0]
}

filter x509_enddate {
  program "openssl"
  args ["x509", "-noout", "-enddate"]
  jail { mount system }
  takes text
  yields text
  ok_codes [0]
}

filter epoch_of {
  program "date"
  in stamp: string
  args ["-u", "-d", stamp, "+%s"]
  jail { mount system }
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
- No `needs` clauses are present because the flow touches no files of its own. Each filter's `jail { mount system }` gives `openssl` and `date` the system set (which includes `/etc/ssl`, `/etc/resolv.conf` and `/etc/hosts`) and nothing else. Their capability set is `{openssl, date}`. The network reach of `openssl` is not bounded by the jail ([section 7.5](#75-not-covered)).
- `classify` is a `fn`, so it is testable without stubs.

## 15. Grammar Sketch

This is a sketch, not a complete grammar. Whitespace is insignificant except as a token separator; newlines terminate statements, and a line ending in `,`, an operator or an open bracket continues. Comments begin with `//`.

```ebnf
file        = { use_decl | declaration } ;
use_decl    = "use" string ;

declaration = flow | filter | fn | test ;

flow        = [ "main" | "private" ] "flow" ident "{" { flow_item } "}" ;
flow_item   = in_decl | out_decl | needs | jail | requires | lock
            | finally | statement ;

in_decl     = "in" ident ":" type [ "=" expr ] ;
out_decl    = "out" ident ":" type ;
needs       = "needs" ( "read" | "write" ) expr ;
jail        = "jail" "{" { mount } "}" ;
mount       = "mount" ( "system" | expr ( "ro" | "rw" ) ) ;
requires    = "requires" expr ;
lock        = "lock" expr ;
finally     = "finally" block ;

filter      = "filter" ident "{" { filter_item } "}" ;
filter_item = "program" string
            | in_decl
            | "args" list_lit
            | jail
            | "env" "{" { ident "=" expr } "}"
            | "takes" io_kind
            | "yields" io_kind
            | "ok_codes" list_lit ;
io_kind     = "none" | "lines" | "text" | "json" ;

fn          = "fn" ident "(" [ params ] ")" "->" type block ;
test        = "test" string "{" { test_item } "}" ;

statement   = let | var | assign | set | if | if_let | for | try_stmt
            | return | fail | step | expr ;
let         = "let" ident [ ":" type ] "=" expr ;
var         = "var" ident [ ":" type ] "=" expr ;
assign      = ident "=" expr ;
set         = "set" ident "=" expr ;
if          = "if" expr block { "else" "if" expr block } [ "else" block ] ;
if_let      = "if" "let" ident "=" expr block [ "else" block ] ;
for         = "for" ident "in" expr block ;
try_stmt    = "try" block { "catch" ident ":" ident block } ;
return      = "return" expr ;
fail        = "fail" "message" ":" expr ;

step        = ( call | ident "{" body "}" ) [ "as" expr ] { modifier } ;
modifier    = "when" expr | "confirmable" ;

expr        = postfix ;
postfix     = primary { "." ident [ "(" [ arg { "," arg } ] ")" ] } ;
                                             (* with parens: a stage (native, filter or run);
                                                without: member access.
                                                A line starting with "." continues the previous one. *)
primary     = call | ident | literal | "(" expr ")" ;
call        = [ "try" ] ident "(" [ arg { "," arg } ] ")" ;
arg         = ident ":" expr | "..." "$" ident ;
type        = ident [ "<" type { "," type } ">" ] [ "?" ]
            | "{" ident ":" type { "," ident ":" type } "}" ;
```

Operator precedence, high to low: member access `.`, unary `not` and `-`, `*` `/` (arithmetic and path join), `+` `-`, comparison, `and`, `or`. Chaining with `.` binds tightest, as member access does.

## 16. Reserved Words

```text
absent   and      args     as       catch    confirmable
else     enum     env      expect   false    fail
filter   finally  fixture  flow     fn       for
if       in       it       jail     json     let
lines    lock     main     mount    needs    none
not      ok_codes or       out      private  program
read     requires return   ro       run      rw
secret   set      stub     system   takes    test
text     true     try      use      var      when
write    yields
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
- **Direct process spawning.** Commands are spawned with an argument vector and no shell. On Linux each spawn goes through the jail helper (`weir __jail-exec`, a re-execution of the same binary), which creates the user, mount and PID namespaces, bind-mounts exactly the mounts the script declared (plus the fixed `/tmp`, `/dev` and `/proc`), rebuilds the environment, calls `pivot_root`, and `exec`s the target. The helper is needed because `unshare(CLONE_NEWUSER)` requires a single-threaded process. Namespace and mount calls go through `libc` or `nix`. Dependency discovery for `weir jail suggest` reads the ELF headers directly rather than invoking `ldd`, which executes the binary.
- **Alternative considered: Go.** Equally capable of a static binary and quicker to write. Rust is preferred for the sum types and exhaustive matching that the checker's data model (types, capabilities, findings) benefits from. This is a preference, not a hard constraint, and the choice does not affect the language.

## 19. Open Questions and Known Weaknesses

### 19.1 No training data, so error messages are the product

Weir does not exist, so no model has seen it. A model asked to write Weir will write something that looks like Rust, Python or bash, and get it wrong in ways particular to Weir. The language cannot rely on familiarity. It can only rely on the check pass saying exactly what is wrong and how to fix it, in a form a model can act on in one step (`fix` hints, stable codes, JSON output).

This means error-message quality is a feature to be tested and measured, not polished at the end. A concrete proposal: before freezing the grammar, collect a set of plausible incorrect scripts written by models and people, run `weir check` on them, and revise the grammar or the messages wherever the fix hint does not lead to a correct script on the next attempt. Whether this works is unproven.

The named-argument rule helps here (there are fewer ways to be wrong) but does not solve it.

### 19.2 Language surface versus "simpler than bash"

The goal is a language simpler than bash for the target scripts. Counting the concepts in this document: four declarations, nine header or step modifiers, eleven types and forms, structured errors, capability declarations with intersection semantics, a mandatory filesystem jail that the script defines mount by mount, with an escape flag, a lockfile, a fixture system, and a native-stage vocabulary. That is not obviously smaller than bash plus a linter, and a reader could reasonably call it larger. Bash is bad, but it is small.

The bet is that these concepts are each individually easy and rarely interact, and that the checker removes the need to learn the sharp edges. That is unverified. Candidates for removal if the surface proves too large: `lock` (a built-in could do it), `requires` (an `if ... fail` does the same), records as a distinct type (use maps), the `finally` block.

### 19.3 Things added beyond the original brief

- **Structural record types** appear in the type table. They are needed for `count_by` output and parsed JSON, but they make Weir's type system larger than the "distinct types" list in the brief. An alternative is `map<string, T>` throughout, at the cost of heterogeneous rows.
- **`set` for outputs and `try` as an attempt record** are syntactic choices made here without much competition from alternatives.
- **The filesystem jail** ([section 7.6](#76-filesystem-jail)) and **`jail` clauses with `mount` on filters and flows** ([section 7.3](#73-intersection-and-filter-mounts)) are not in the original brief. They follow from the decision that external programs must be confined by the same declaration as built-ins.
- **`$` in `...$list`** appears only in spread. Variables are otherwise bare. It is inconsistent and should probably be `...list`.

### 19.4 Deferred past v1

- Parallelism (concurrent steps or `for` iterations).
- Network capabilities (allowed hosts, ports).
- Jail refinements ([section 7.6](#76-filesystem-jail)): overlayfs-backed `--plan` and commit-on-success, a Landlock backend where user namespaces are disabled, network namespaces.
- Jail support outside Linux (macOS, BSDs). v1 refuses to start external programs where it cannot jail them.
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
- **The jail is Linux-only and may be unavailable.** It depends on unprivileged user namespaces, which are disabled on some hardened hosts and forbidden in some containers. There the program is not started (exit 4) unless the operator passes `--no-jail`. macOS and the BSDs have no v1 jail, so any script that runs an external program is Linux-only in practice. For a language meant to replace bash everywhere, this is a significant limit. Whether to accept it, or to add a weaker non-Linux backend, is open.
- **Script-defined mounts are verbose and easy to get wrong.** Programs need paths nobody thinks of: certificate stores, locale data, NSS modules, helper binaries, daemon sockets. Because the author lists the mounts, a missing one is the author's bug and shows up as a program failing to start or to find a library. `mount system`, the `E-JAIL-EXEC-UNREACHABLE` check and `weir jail suggest` are the mitigations. If authors find this too burdensome they will either mount broadly (a flow cannot mount `/` read-write, but `/usr`, `/etc` and `/var` are not so limited) or run with `--no-jail`, and the guarantee shrinks to nothing. Whether explicit mounts or automatic derivation yields better real-world jails is open; the design chose explicit and records the alternative in section 7.6.
- **Fixtures are under-specified.** Stubbing at the filter level does not test the real program's behaviour, and the fixture language for directory trees is only sketched.
- **Naming strictness has a cost.** Named-only calls make short scripts noticeably longer than their bash equivalents. This may hurt adoption for one-liners, which are a large share of what bash is used for. A `weir -e` one-liner mode is not designed.

### 19.6 Checker rules that are expensive to implement

Roughly in order of cost:

1. **Proving a write is inside the declared scope.** Paths are computed from inputs and strings (`dir / name`). A sound static proof needs symbolic path reasoning: normalising `..`, tracking that a value derives from a `needs` root, and handling symlinks, which static analysis cannot see at all. The design therefore does not attempt it: scope is decided at runtime on evaluated paths ([section 7.1](#71-filesystem-scope-is-declared-and-enforced-at-runtime)), and the static check reports only violations provable from literals. The cost is that some violations surface at runtime, as exit 4 partway through a run, rather than at check time.
2. **Capability inference through the call graph** with imports from remote components. It requires computing the manifest of each component without executing it, and it must handle recursion and diamond imports.
3. **Pipeline element-type inference** through generic stages with `it`. Requires bidirectional inference and good error messages when it fails, which is hard because the error appears far from the cause.
4. **`secret` flow analysis.** Tracking that no expression derived from a `secret` reaches an argument list, an interpolation, a file write or an output. Has to follow values through `fn` returns and record fields.
5. **`W-DEFAULT-ARG` with constant folding.** Detecting that an argument equals the default needs evaluation of constant expressions, including durations and path joins.
6. **Exhaustiveness of `catch` and reachability of thrown types.** Needs each built-in and each filter to declare which errors it may throw.
7. **Dependency discovery for `weir jail suggest`.** Reading ELF headers for `DT_NEEDED` entries, following `RPATH`/`RUNPATH` and the loader search path, resolving `#!` chains, and doing so identically to what the dynamic loader will do at run time. Anything loaded with `dlopen` is invisible and has to be found by the author. It is not on the run-time path, so a mistake costs a bad suggestion, not a wrong jail. The `E-JAIL-EXEC-UNREACHABLE` check is a smaller version of the same problem: resolving `program` through `PATH` and symlinks the way the runtime will.
8. **Purity checking of `fn`.** Cheap on its own, but must be re-run through every imported `fn` and built-in, so every built-in needs a purity annotation that stays correct.

### 19.7 Questions not yet decided

- Should `use` allow importing a `fn` or `filter` on its own, or only flows? The manifest model currently assumes flows, but filters are the most reusable thing.
- How are exit codes from nested flow calls reflected in `--json` `steps`? One failure should not produce a confusing cascade of "failed" entries.
- Is the `lock` path part of the capability model? As written it is not subject to `needs write`, which is an exception worth removing.
- Should `ok_codes` allow named meanings (for example, `grep` code 1 meaning "no match" and so `yields` an empty result), rather than only a list of numbers?
