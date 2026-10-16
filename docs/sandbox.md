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
`sandbox` block mounted. A filter with no `see` lines sees everything the script mounted, except explicit mounts (below).
A `see` line can only narrow: `see "/etc/nginx" rw` would be an error here, because the
script mounts it read-only. Only the script you run has a `sandbox` block. A script it
imports with `use` can't mount anything, so imported code never sees more than the
script you run allows.

## Mounts only a request can reach

Some mounts are too powerful to give every program in the script, such as the Docker
socket or a folder of keys. Mark them `explicit`:

```weir
sandbox {
  mount system
  mount "/etc/ssl/private" ro explicit
}
```

A explicit mount is still declared in the `sandbox` block, but it isn't part of the
default view. The script's built-ins, a generic `run`, and a filter with no `see` lines
can't reach it: for a program it is simply missing, for a built-in it is a
`CapabilityError` that names the mount, and `exists` says `false`. A program sees it
only if its filter says so, or it runs in a `within` block that does:

```weir
filter sign {
  program "openssl"
  in file: path
  args ["dgst", "-sha256", "-sign", "/etc/ssl/private/release.key", file]
  see system
  see "/etc/ssl/private" ro      // the request
  takes none
  yields lines
}
```

The request has to name the mount or a path inside it. `see "/etc/ssl" ro` or a
`within` for the parent folder doesn't include it. A request can't widen the mode, so
a `ro` mount stays `ro`.

Once a script has a explicit mount, every program it starts gets its own narrowed
view, which costs about 0.6 ms per program. One caveat: restriction applies to the
mount path. If another mount exposes the same host folder, the folder is visible
there. Without a sandbox (`--sandbox off`) programs aren't confined at all, so only
built-ins honour `explicit`.

## How it is enforced

On Linux the interpreter creates user, mount and PID namespaces without root,
builds a tmpfs root from bind mounts, and calls `pivot_root`. This is not a
`chroot`: the old root is detached from the process, so there is no path back to it.

Inside, a program sees only its mounts, a private `/tmp`, a minimal `/dev`, and
its own `/proc`. A `ro` mount rejects writes. Other host directories next to a
mounted one are invisible. `--ro` and `--deny-exec` narrow the sandbox for a single
run, and `--sandbox` chooses what happens when it can't be built, as described below.

## When the sandbox cannot be created

Some hosts forbid user namespaces: hardened kernels, and containers whose seccomp
profile blocks `unshare` and `pivot_root`. The `--sandbox` option, or the
`WEIR_SANDBOX` environment variable, says what to do then. The option wins over the variable.

| Mode | A sandbox can be built | It cannot |
|---|---|---|
| `required` (the default) | the script runs inside it | return code 4, nothing is run |
| `auto` | the script runs inside it | a warning naming the cause, then the script runs without it |
| `off` | the script runs without it, with a warning | the same |

`auto` is for a machine that is already isolated, such as a CI job in a container.
Weir tries to build the sandbox in a short-lived child process before the script
starts, so a script never begins inside a half-built one. The `--json` result says
`"sandbox": false` whenever the script ran without one.

Without a sandbox every path in the script is a host path, so a script with a
mount that remaps a path (`mount logs at "/logs" ro`) can't run that way. That is
return code 2 under `off`, and return code 4 when `auto` fell back because the
sandbox could not be built. `--no-sandbox` is the same as `--sandbox off`.

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
