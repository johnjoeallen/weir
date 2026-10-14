# Weir

Scripts that glue programs together, checked before they run.

<div class="weir-actions">
  <a class="weir-button" href="getting-started/">Get started</a>
  <a class="weir-button weir-button-secondary" href="https://github.com/johnjoeallen/weir">GitHub</a>
</div>

Weir is a small interpreted language for anything you would otherwise write in
bash and run from a terminal, a cron job, a systemd unit or a pipeline. A script
has a shape: inputs, a sequence of steps, some outputs. Shell languages leave
that shape implied. Weir makes it the language, and checks it before anything runs.

```weir
sandbox {
  mount system
  mount "/var/log" ro                 // the only part of the machine this script can see
}

main flow report {
  in  log: path = path(text: "/var/log/dpkg.log")
  in  since: duration = 7d
  in  top: int = 10
  out changes: int

  let events = load(log: log).events
    .filter(where: it.epoch >= now_epoch() - since.seconds)

  summary {
    for p in events.count_by(key: it.package).sort_by(key: it.count, order: desc).take(n: top) {
      print(text: "${p.key}\t${p.count}")
    }
  }
  set changes = events.count()
}
```

## What you get

- **Typed inputs become the command line.** `--help` is generated, and a bad
  value is rejected before the first step runs (`weir` exits with return code 2).
- **Commands take argument lists, not strings.** There is no shell, so quoting
  bugs cannot be written.
- **Every script declares the filesystem it runs in.** The `sandbox` block is
  enforced with Linux namespaces, including for every program the script starts.
  Nothing prompts.
- **`weir check` finds problems first.** Each one has a stable code, a position
  and a fix hint, in terms of what you wrote.
- **Steps that change things are `confirmable`.** They are skipped, and reported
  as skipped, unless the caller passes `--confirm`.
- **Tests are part of the file.** `weir test` runs them with program output stubbed.

## Status

!!! warning "Early, and subject to change"
    The interpreter runs the examples in this repository and the worked
    examples in the design document. The language, its syntax and its tooling are
    subject to change. [Implementation status](IMPLEMENTATION.md) lists what is
    built, what is not, and the decisions made where the design was silent.
