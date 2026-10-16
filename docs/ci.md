# Using Weir in CI

A Weir script is a command line program, so a CI job runs it as one step. The
CI system decides when a job runs. The script decides what the job does.

## A workflow

```yaml
name: nginx-check
on: [push]

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Weir
        run: cargo install --git https://github.com/johnjoeallen/weir --tag v0.3.0

      - name: Check and test
        run: |
          weir check ci/deploy.weir
          weir test ci/deploy.weir

      - name: Run
        env:
          WEIR_SECRET_TOKEN: ${{ secrets.DEPLOY_TOKEN }}
        run: weir run ci/deploy.weir --json
```

`weir check` and `weir test` need no sandbox and read no secrets, so they work on
any runner. `weir test` stubs every program, so it runs nothing real.

## What a step can rely on

- **Return codes.** 0 is success and 1 is a failed flow. 2 is a usage error and 3 means
  `weir check` found errors. 4 means a capability was denied, including a sandbox that
  could not be created. A step fails on any non-zero code, and the number says why.
- **Secrets.** A `secret` input is read from `WEIR_SECRET_<NAME>`, `--secrets-file` or
  `--secrets-stdin`, never from an option, so it doesn't appear in the process list.
  See [Secrets](secrets.md).
- **No terminal.** Steps marked `confirmable` are skipped unless `--confirm` is given, and
  `--plan` prints what would happen without running it.
- **Machine-readable results.** `--json` prints one document with the outputs, the
  steps and `"sandbox": true` or `false`. `weir check --json` does the same for errors.

## When the runner can't build the sandbox

The sandbox needs unprivileged user namespaces. Some runners forbid them: a hardened
kernel, an AppArmor setting, or a container job whose seccomp profile blocks `unshare`
and `pivot_root`. With the default mode, `required`, the step then fails with return
code 4 and the message names the cause.

If the job is already isolated, as in a container, set the mode once for the job
instead of changing the script:

```yaml
jobs:
  deploy:
    runs-on: ubuntu-latest
    container: debian:stable
    env:
      WEIR_SANDBOX: auto
```

- **`auto`** builds the sandbox when it can. When it can't, the log shows a warning with
  the cause and the script runs without it. The result records `"sandbox": false`.
- **`off`** never builds it. Use it when you know the runner can't, so the log doesn't
  carry a probe failure on every run.
- A script can't set its own mode, so a pull request that edits the script can't turn the
  sandbox off. The mode is part of the workflow file, which reviewers can see.

Without a sandbox the script's `mount` lines don't restrict anything, and paths
aren't remapped. A script with a mount that remaps a path (`mount logs at "/logs" ro`)
can't run that way. See [When the sandbox cannot be created](sandbox.md#when-the-sandbox-cannot-be-created).

!!! note "Not yet verified on hosted runners"
    Whether GitHub-hosted runners allow unprivileged user namespaces depends on the
    runner image and its kernel settings, and has not been checked here. Run
    `weir run` once with the default mode to see. If it exits with return code 4, use `auto`.
