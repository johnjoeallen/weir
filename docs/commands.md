# Running programs

Weir starts a program as an argument list. There is no shell, so nothing is split on
spaces, expanded or re-parsed, and the quoting bugs that bash scripts have cannot be
written. Every snippet on this page comes from
[`examples/commands.weir`](https://github.com/johnjoeallen/weir/blob/main/examples/commands.weir),
which you can run with `weir run examples/commands.weir`.

There are two ways to start a program: a declared **tool**, and the generic **`run`**.

## A declared tool

A tool wraps exactly one program. It names the program, its typed parameters, how the
program reads input and what it produces, and the part of the disk it can see.

```weir
tool cat_file {
  program "cat"
  in file: path
  args ["--", file]          // a list; "--" stops a file named "-x" being read as an option
  see system
  see "/etc" ro            // this program can see /etc and nothing else the script mounted
  takes none                 // stdin is closed
  yields lines               // stdout becomes a list of lines
}
```

Call it like a function, with named arguments:

```weir
let passwd = cat_file(file: path(text: "/etc/passwd"))
```

`takes` and `yields` are each one of `none`, `lines`, `text` or `json`. A tool that
`takes lines` reads the lines before it in a chain:

```weir
tool grep_fixed {
  program "grep"
  in pattern: string
  args ["-F", "-e", pattern]
  see system
  takes lines
  yields lines
  ok_codes [0, 1]            // grep returns 1 for "no match", which is not a failure here
}

set found = passwd.grep_fixed(pattern: pattern)
```

The `.` joins them. The lines from `passwd` become the standard input of `grep`.

## The generic `run`

For a one-off call that doesn't need a declaration:

```weir
let kernel = run(cmd: "uname", args: ["-s", "-r"])
```

It returns the program's output as a list of lines. `cmd` must be a literal string, so
the program is known before the script runs, and `weir check` can test it against the
sandbox. A computed program name is the error `E-DYNAMIC-CMD`.

Spread a list into the arguments with `...$name`:

```weir
let words = ["one", "two", "three"]
let echoed = run(cmd: "echo", args: ["words:", ...$words])
```

!!! warning "No shell strings"
    `run(cmd: "tar -czf a b")` doesn't split the string. It looks for a program with that
    whole text as its name. Starting a shell (`sh`, `bash`) and passing it a string is
    allowed but `weir check` warns about it (`W-SHELL-STRING`), because the string is
    re-parsed, which is what Weir is designed to avoid.

## When a program fails

A return code outside `ok_codes` throws `ExecError`. The default is `[0]`, so a failure
stops the script unless you say otherwise. There is nothing like `set -e` to forget.

Three ways to deal with it:

**List the return codes that are fine.**

```weir
// grep -c prints a count, and returns 1 when the count is zero.
let no_match = run(cmd: "grep", args: ["-c", "-F", "-e", "no-such-text-zzz", "/etc/hostname"], ok_codes: [0, 1])
```

**`try`** turns a failure into a value. The attempt has `ok`, `code` and `message`, and
`try` itself never throws:

```weir
let attempt = try run(cmd: "false", args: [])
print(text: "false ${describe(code: attempt.code)}")
```

**`catch`** handles a typed error:

```weir
try {
  cat_file(file: path(text: "/etc/no-such-file"))
} catch e: ExecError {
  print(text: "cat failed as expected: ${e.message}")
}
```

`fail message: "..."` stops the script with return code 1 and your message.

## Restricting several commands: `within`

A tool's `see` lines narrow one program. To narrow a stretch of the script, put it in a `within`
block, which can also return a value:

```weir
let info = within system, "/etc" ro {
  let kernel = run(cmd: "uname", args: ["-s", "-r"])
  let hosts  = cat_file(file: path(text: "/etc/hosts"))
  return { kernel: first(of: kernel), hosts: count(of: hosts) }
}
```

Inside the block `/var/log` doesn't exist for any program or built-in, even though the script mounts it.
See [The sandbox](sandbox.md#narrowing-part-of-a-flow-within).

## Secrets

A tool gets a `secret` input through its `env` block or its stdin, never through `args`.
See [Secrets](secrets.md).

## Testing

A test never starts a real program. It gives each tool the lines it would yield:

```weir
test "search finds the matching lines" {
  stub cat_file yields lines: ["root:x:0:0", "daemon:x:1:1", "root2:x:2:2"]
  stub grep_fixed yields lines: ["root:x:0:0", "root2:x:2:2"]
  let r = run search(pattern: "root")
  expect count(of: r.found) == 2
}
```

A stub can name arguments, and then it answers only calls that pass those values.
Several stubs for the same tool answer in turn: the first call gets the first
matching stub, the second call the second, and the last one answers every call after that.

```weir
test "the second look sees the new container" {
  stub compose(words: ["ps", "--quiet", "web"]) yields lines: ["old"]
  stub compose(words: ["ps", "--quiet", "web"]) yields lines: ["old", "new"]
  stub docker(words: ["rm", "old"]) yields none
  ...
}
```

A stub with no arguments matches every call, so it counts in the turns too. A call that
no stub matches fails the test and shows the arguments it was made with. A tool that
yields `none` is stubbed with `yields none` and no value.

A tool without a stub fails the test and tells you the line to add. The generic `run`
has no stub, so put the calls you want to test in tools, and keep one-off `run` calls
in flows that the tests don't call.

## Programs that change things

A step that changes the system goes in a `confirmable` block. It's skipped, and reported
as skipped, unless the caller passes `--confirm`. See `examples/logwatch.weir`, which edits
a deny file and reloads nginx this way.

The [design document](DESIGN.md#6-commands-and-errors) has the full rules.
