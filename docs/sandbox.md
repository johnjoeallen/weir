# The sandbox

The `sandbox` block at the top of the entry file defines the filesystem the
script runs in. The interpreter and every program it starts live inside it, so
every path in the file is a path inside the sandbox, and nothing that is not
mounted exists.

```weir
sandbox {
  mount system                      // the usual program directories, read-only
  mount "/var/log/nginx" rw
  mount "/etc/nginx" ro
}
```

A filter narrows what its own program sees with `sandbox { see ... }`. A
component cannot widen what the entry file allows.

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
