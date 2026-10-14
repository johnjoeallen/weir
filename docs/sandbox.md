# The sandbox

A script declares the part of the machine it can see in a `sandbox` block at the
top of the script you run. Weir builds that view first and then runs the script
inside it, together with every program the script starts. A directory the block
doesn't mount isn't hidden or read-only. It simply isn't there. Paths written in the
script refer to this view, not to the host.

```weir
sandbox {
  mount system                      // the usual program directories, read-only
  mount "/var/log/nginx" rw
  mount "/etc/nginx" ro
}
```

A filter can restrict what its program sees by adding `see` lines to its declaration.
This one runs `nginx -t`, which needs the config and its own error log:

```weir
filter nginx_test {
  program "nginx"
  args ["-t"]
  see system
  see "/etc/nginx" ro
  see "/var/log/nginx" rw       // nginx -t opens its error log
  takes none
  yields none
}
```

The program sees only the paths listed there, and only those that the script's own
`sandbox` block mounted. A filter with no `see` lines sees everything the script mounted.
A `see` line can only narrow: `see "/etc/nginx" rw` would be an error here, because the
script mounts it read-only. Only the script you run has a `sandbox` block. A script it
imports with `use` can't mount anything, so imported code never sees more than the
script you run allows.

## How it is enforced

On Linux the interpreter creates user, mount and PID namespaces without root,
builds a tmpfs root from bind mounts, and calls `pivot_root`. This is not a
`chroot`: the old root is detached from the process, so there is no path back to it.

Inside, a program sees only its mounts, a private `/tmp`, a minimal `/dev`, and
its own `/proc`. A `ro` mount rejects writes. Other host directories next to a
mounted one are invisible. `--ro` and `--deny-exec` narrow the sandbox for a single
run, and `--no-sandbox` removes it (with a warning).

## A smaller view for one program

A filter's `see` lines give its program a second, smaller root
inside the script's sandbox. Just before the program starts, Weir:

1. gives the child process its own copy of the mount table (a nested mount namespace);
2. builds a fresh tmpfs root and bind-mounts only the `see` paths into it, as `ro` or
   `rw` as written, with a private `/tmp`, a minimal `/dev` and `/proc`;
3. calls `pivot_root` into that root and detaches the previous one;
4. starts the program.

```text
host
 └─ the script's sandbox      user, mount and PID namespaces, pivot_root
     └─ one filter's view     nested mount namespace, pivot_root, only the `see` paths
```

Paths the script mounted but the filter doesn't `see` aren't in that program's
view at all. The script, and any other filter, still sees them. A `see` can only
narrow: it can't name a path the script didn't mount (`E-SEE-UNMOUNTED`), and
`see "/x" ro` makes a `rw` mount read-only for that program.

The nested view changes the filesystem only. The program stays in the same user
and PID namespaces as the script.

## Narrowing part of a flow: `within`

A `within` block does for several statements what `see` lines do for one program. Its header lists
the view, and every program started inside it, and the built-ins that touch the filesystem,
see only that:

```weir
let info = within system, "/etc" ro {
  let kernel = run(cmd: "uname", args: ["-s", "-r"])
  let hosts  = cat_file(file: path(text: "/etc/hosts"))

  return { kernel: first(of: kernel), hosts: count(of: hosts) }
}
```

- **A value:** `return` leaves the block with a value, so no variable has to be declared outside
  it. A block used as a value must return on every path (`E-WITHIN-NO-RETURN`). Variables declared
  inside the block stay inside it.
- **Only narrower:** a `within` can't widen what the script, or an enclosing `within`, allows. Naming a
  path the script never mounted is `E-SEE-UNMOUNTED`; naming one the enclosing block doesn't have
  fails when the block starts.
- **Programs:** a path outside the view is missing, as it would be for a filter with `see` lines.
- **Built-ins:** `read_text`, `write` and the others fail with an error naming the `within` block.
  `exists` returns `false`.
- **Cost:** each program gets its own narrowed root, which adds about 0.6 ms.

The word `sandbox` is used only for the block at the top of the script. A filter uses `see` lines,
and a flow uses `within`.

## Limits

- Linux with unprivileged user namespaces only. Tested on Linux 6.12, x86_64.
- The network is outside the sandbox.
- The interpreter reaps only the children it starts.

See [Implementation status](IMPLEMENTATION.md#known-limits-of-the-sandbox) for the
full list.
