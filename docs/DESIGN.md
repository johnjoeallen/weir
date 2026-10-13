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
7. [Capabilities](#7-capabilities)
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
5. **Effects are declared.** What a script may read, write and execute is visible from its header, and a caller can bound it.
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
| 4    | capability denied (an operation outside the granted capabilities)    |

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

### 7.1 Filesystem capabilities are declared

A flow declares what it reads and writes with `needs read` and `needs write`. Paths may use inputs: `needs write dir`. A built-in that touches the filesystem outside the declared scope throws `CapabilityError` (exit 4), and the checker rejects it statically when it can prove the violation.

### 7.2 Executable capabilities are inferred

The set of executables a flow may run is computed from its call graph: the `program` of each filter it reaches, and the literal `cmd` of each `run`. A dynamic `cmd` (computed at runtime) is a check error, which is the reason `program` and `cmd` must be literals.

**Alternative rejected: requiring an explicit `needs exec` list.** It is redundant with the call graph and goes stale; an inferred set cannot disagree with the code.

**Alternative considered: inferring filesystem capabilities too.** Rejected for v1: paths are frequently computed from inputs, so inference would produce either imprecise (everything) or unsound results. Declaring them is also documentation for the reader and the caller.

### 7.3 Intersection

A called component receives the intersection of its own declaration and the caller's grant. A library flow that declares `needs write "/"` cannot exceed what its caller allowed, so importing a component cannot widen a script's reach. The caller's grant is the caller's declaration intersected with anything imposed from the CLI (`--allow-write PATH`, `--deny-exec NAME`).

### 7.4 Destructive built-ins

The built-ins `delete` and `move` refuse any path outside the declared write scope, and unconditionally refuse `/` and the user's `$HOME`. This is a backstop for the empty-variable `rm -rf "$DIR/"` case. Because `path` values are never concatenated from strings and an unset input is `absent` rather than empty, most such cases are caught earlier by the type system.

### 7.5 Not covered

Network access is not a declared capability in v1. It is bounded only indirectly, through which executables may run. This is a known gap ([section 19](#19-open-questions-and-known-weaknesses)).

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

`|` chains stages. A stage is a native stage, a declared filter, or a bare `run` command. A failing stage fails the pipeline, always. There is no `pipefail` switch because there is no mode in which a failure is ignored.

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
  | filter(where: contains(text: it, part: "Failed password"))
  | map(to: ip_of(line: it))
  | count_by(key: it)
  | sort_by(key: it.count, order: desc)
  | take(n: 10)
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
```

### 11.1 Generated flags

Typed inputs generate flags and `--help`. `in count: int = 3` becomes `--count <int>` with `3` shown as default. `list<T>` inputs accept repeated flags. `bool` inputs are `--flag` / `--no-flag`. Enums list their values in `--help`. A value that fails to parse as its declared type is exit 2 with the offending flag named.

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

Diagnostics and logs always go to stderr. Outputs assigned before a failure appear in the `--json` document, so a caller can see partial results.

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
  takes none
  yields lines
}

filter nginx_test {
  program "nginx"
  args ["-t"]
  takes none
  yields none
}

filter systemctl {
  program "systemctl"
  in action: enum(reload, restart, status)
  in unit: string
  args [action, unit]
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
  let rules = ips | map(to: "deny ${it};")
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
  requires exists(path: log)
  lock "/run/lock/ban_noisy_clients.lock"

  var offenders: list<string> = []

  find_offenders {
    let found = tail(file: log, n: window)
      | filter(where: status_of(line: it) >= 400)
      | count_by(key: ip_of(line: it))
      | filter(where: it.count >= threshold)
      | sort_by(key: it.count, order: desc)
      | map(to: it.key)
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

- `needs write blocklist` bounds `write` and `copy`. The backup file lands next to the blocklist, so it is within the declared scope only if the declaration covers the parent directory. As written, the checker flags `copy(to: backup)` as outside `needs write blocklist` and suggests `needs write blocklist.parent()`. This is a deliberate sample of the kind of error message the checker must get right.
- The inferred executable set is `{tail, nginx, systemctl}`. A caller can see this with `weir list --manifest`.
- The flow is safe to run from cron. Without `--confirm` it updates the blocklist and validates it, then reports `reload: skipped: unconfirmed`. An operator can follow with `weir run ban_noisy_clients.weir --confirm`, or put `--confirm` in the cron line once they trust it.
- If `nginx_test()` fails the flow fails with exit 1 and the `.bak` file is left in place. A production version would restore the backup in a `catch`; it is omitted here to keep the example short.
- `tail ... | filter ... | count_by ...` streams until `count_by`, which buffers one counter per distinct IP, then `sort_by` buffers the resulting (smaller) set. The log itself is never held in memory.

## 14. Worked Example: Certificate Expiry Checker

Probe a list of hosts, compute days to certificate expiry, classify each, and fail if any is critical or unreachable. It is intended to run from cron and emit JSON for a monitoring system.

```weir
// check_certs.weir
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
      let attempt = try (s_client(host: host, port: port) | x509_enddate())
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
    | filter(where: it.state == "critical" or it.state == "unreachable")
    | count()
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
- No `needs` clauses are present because the flow touches no files. Its capability set is `{openssl, date}` and nothing else. The network reach of `openssl` is not bounded by this declaration ([section 7.5](#75-not-covered)).
- `classify` is a `fn`, so it is testable without stubs.

## 15. Grammar Sketch

This is a sketch, not a complete grammar. Whitespace is insignificant except as a token separator; newlines terminate statements, and a line ending in `,`, an operator or an open bracket continues. Comments begin with `//`.

```ebnf
file        = { use_decl | declaration } ;
use_decl    = "use" string ;

declaration = flow | filter | fn | test ;

flow        = [ "main" | "private" ] "flow" ident "{" { flow_item } "}" ;
flow_item   = in_decl | out_decl | needs | requires | lock
            | finally | statement ;

in_decl     = "in" ident ":" type [ "=" expr ] ;
out_decl    = "out" ident ":" type ;
needs       = "needs" ( "read" | "write" ) expr ;
requires    = "requires" expr ;
lock        = "lock" expr ;
finally     = "finally" block ;

filter      = "filter" ident "{" { filter_item } "}" ;
filter_item = "program" string
            | in_decl
            | "args" list_lit
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

expr        = pipeline ;
pipeline    = ternary { "|" stage } ;
stage       = call | ident ;                 (* native, filter or run *)
call        = [ "try" ] ident "(" [ arg { "," arg } ] ")" ;
arg         = ident ":" expr | "..." "$" ident ;
type        = ident [ "<" type { "," type } ">" ] [ "?" ]
            | "{" ident ":" type { "," ident ":" type } "}" ;
```

Operator precedence, high to low: member access `.`, unary `not` and `-`, `*` `/` (arithmetic and path join), `+` `-`, comparison, `and`, `or`, `|`.

## 16. Reserved Words

```text
absent   and      args     as       catch    confirmable
else     enum     env      expect   false    fail
filter   finally  fixture  flow     fn       for
if       in       it       let      lines    lock
main     needs    none     not      ok_codes or
out      private  program  read     requires return
run      secret   set      stub     takes    test
text     true     try      use      var      when
write    yields   json
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
- **Direct process spawning.** Commands are spawned with `std::process::Command` and an argument vector. No shell is involved at any point in the interpreter.
- **Alternative considered: Go.** Equally capable of a static binary and quicker to write. Rust is preferred for the sum types and exhaustive matching that the checker's data model (types, capabilities, findings) benefits from. This is a preference, not a hard constraint, and the choice does not affect the language.

## 19. Open Questions and Known Weaknesses

### 19.1 No training data, so error messages are the product

Weir does not exist, so no model has seen it. A model asked to write Weir will write something that looks like Rust, Python or bash, and get it wrong in ways particular to Weir. The language cannot rely on familiarity. It can only rely on the check pass saying exactly what is wrong and how to fix it, in a form a model can act on in one step (`fix` hints, stable codes, JSON output).

This means error-message quality is a feature to be tested and measured, not polished at the end. A concrete proposal: before freezing the grammar, collect a set of plausible incorrect scripts written by models and people, run `weir check` on them, and revise the grammar or the messages wherever the fix hint does not lead to a correct script on the next attempt. Whether this works is unproven.

The named-argument rule helps here (there are fewer ways to be wrong) but does not solve it.

### 19.2 Language surface versus "simpler than bash"

The goal is a language simpler than bash for the target scripts. Counting the concepts in this document: four declarations, nine header or step modifiers, eleven types and forms, structured errors, capability declarations with intersection semantics, a lockfile, a fixture system, and a native-stage vocabulary. That is not obviously smaller than bash plus a linter, and a reader could reasonably call it larger. Bash is bad, but it is small.

The bet is that these concepts are each individually easy and rarely interact, and that the checker removes the need to learn the sharp edges. That is unverified. Candidates for removal if the surface proves too large: `lock` (a built-in could do it), `requires` (an `if ... fail` does the same), records as a distinct type (use maps), the `finally` block.

### 19.3 Things added beyond the original brief

- **Structural record types** appear in the type table. They are needed for `count_by` output and parsed JSON, but they make Weir's type system larger than the "distinct types" list in the brief. An alternative is `map<string, T>` throughout, at the cost of heterogeneous rows.
- **`set` for outputs and `try` as an attempt record** are syntactic choices made here without much competition from alternatives.
- **`$` in `...$list`** appears only in spread. Variables are otherwise bare. It is inconsistent and should probably be `...list`.

### 19.4 Deferred past v1

- Parallelism (concurrent steps or `for` iterations).
- Network capabilities (allowed hosts, ports).
- Windows support. The v1 design assumes POSIX semantics for signals, locks, and paths.
- A package index. Components come from paths, GitHub and Artifactory/Nexus only.
- Interactive use and a REPL.
- Declared timeouts on steps and commands (only `Timeout` as an error type is reserved).
- Resumable runs (re-running only failed steps).
- An editor language server, beyond the JSON output of `weir check`.
- Formal denial of `run(cmd: "bash", ...)`. It is only warned about in v1.

### 19.5 Weaknesses in the design as it stands

- **`--plan` cannot be complete.** It does not execute commands, so any control flow depending on a command's output cannot be resolved. The plan shows both branches annotated with the condition, which can overstate what will happen. A plan is a conservative superset of effects, not a prediction. Whether this is useful enough in practice is open.
- **Pipeline early termination.** Distinguishing "closed by `take`" from a real failure across platforms and programs is delicate. Some programs report a broken pipe with a distinct code, some with a message, some hang. This may need a per-filter declaration.
- **`confirmable` semantics for dependent steps.** If a later step depends on the skipped step's effect, running it is wrong. The current rule only skips the one step. A `needs_step` dependency or a rule that skips all later steps in the flow is needed.
- **`fn` purity versus reading the clock.** `now_epoch()` is not allowed in a `fn`, so time-dependent logic must be passed in as an argument. This is correct and mildly annoying.
- **Secrets in filters.** `env { NAME = token }` is the only route for a secret to a program besides stdin. Some programs accept credentials only on argv. Those cannot be wrapped safely, and the design offers no answer beyond "write a wrapper".
- **Network is outside the capability model.**
- **Fixtures are under-specified.** Stubbing at the filter level does not test the real program's behaviour, and the fixture language for directory trees is only sketched.
- **Naming strictness has a cost.** Named-only calls make short scripts noticeably longer than their bash equivalents. This may hurt adoption for one-liners, which are a large share of what bash is used for. A `weir -e` one-liner mode is not designed.

### 19.6 Checker rules that are expensive to implement

Roughly in order of cost:

1. **Proving a write is inside the declared scope.** Paths are computed from inputs (`dir / name`). A sound check needs symbolic path reasoning: normalising `..`, tracking that a value derives from a `needs` root, and handling symlinks, which static analysis cannot see at all. The practical design is a conservative static check plus a runtime check that is the real guarantee. The static rule will have false positives and the message must say why.
2. **Capability inference through the call graph** with imports from remote components. It requires computing the manifest of each component without executing it, and it must handle recursion and diamond imports.
3. **Pipeline element-type inference** through generic stages with `it`. Requires bidirectional inference and good error messages when it fails, which is hard because the error appears far from the cause.
4. **`secret` flow analysis.** Tracking that no expression derived from a `secret` reaches an argument list, an interpolation, a file write or an output. Has to follow values through `fn` returns and record fields.
5. **`W-DEFAULT-ARG` with constant folding.** Detecting that an argument equals the default needs evaluation of constant expressions, including durations and path joins.
6. **Exhaustiveness of `catch` and reachability of thrown types.** Needs each built-in and each filter to declare which errors it may throw.
7. **Purity checking of `fn`.** Cheap on its own, but must be re-run through every imported `fn` and built-in, so every built-in needs a purity annotation that stays correct.

### 19.7 Questions not yet decided

- Should `use` allow importing a `fn` or `filter` on its own, or only flows? The manifest model currently assumes flows, but filters are the most reusable thing.
- How are exit codes from nested flow calls reflected in `--json` `steps`? One failure should not produce a confusing cascade of "failed" entries.
- Is the `lock` path part of the capability model? As written it is not subject to `needs write`, which is an exception worth removing.
- Should `ok_codes` allow named meanings (for example, `grep` code 1 meaning "no match" and so `yields` an empty result), rather than only a list of numbers?
