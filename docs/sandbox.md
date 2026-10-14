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

## Limits

- Linux with unprivileged user namespaces only. Tested on Linux 6.12, x86_64.
- The network is outside the sandbox.
- The interpreter reaps only the children it starts.

See [Implementation status](IMPLEMENTATION.md#known-limits-of-the-sandbox) for the
full list.
