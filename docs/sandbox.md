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

A filter can restrict what its program sees by adding a `sandbox { see ... }` block.
The program then sees only the paths listed there, and only those that the script's
own `sandbox` block mounted. Only the script you run has a `sandbox` block. A script
it imports with `use` can't mount anything, so imported code never sees more than the
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

A filter's `sandbox { see ... }` block gives its program a second, smaller root
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

## Limits

- Linux with unprivileged user namespaces only. Tested on Linux 6.12, x86_64.
- The network is outside the sandbox.
- The interpreter reaps only the children it starts.

See [Implementation status](IMPLEMENTATION.md#known-limits-of-the-sandbox) for the
full list.
