# Secrets

A `secret` input holds a credential: a token, a password, an access key. Weir
keeps it out of the places a credential usually leaks from: the command line, the
output, and the script's own strings.

```weir
main flow sync {
  in access_key_id: secret
  in secret_access_key: secret
  ...
}
```

A `secret` input has no default (`E-SECRET-DEFAULT`). `secret?` makes it optional.

## Where a secret comes from

There is no `--access_key_id` option. A secret is never accepted on the command
line, because the command line shows up in `ps` and in shell history. Weir looks in
three places, in this order, and uses the first that has a value:

| Order | Source | Use it for |
|---|---|---|
| 1 | `--secrets-file <path>` | a file on disk, such as a mounted secret in a container |
| 2 | `--secrets-stdin` | typing each secret at a prompt, or piping them in from a password manager or vault client |
| 3 | the environment variable `WEIR_SECRET_<NAME>` | CI, where the runner sets variables from its secret store |

`<NAME>` is the input name in upper case, so `access_key_id` is `ACCESS_KEY_ID` in a file or
feed, and `WEIR_SECRET_ACCESS_KEY_ID` in the environment.

## The secrets file and the feed

A secrets file and a stdin feed have the same form, like an environment file:

```text
# aws.env
ACCESS_KEY_ID=AKIA...
SECRET_ACCESS_KEY="a value with spaces"
```

- Blank lines and lines starting with `#` are skipped. `export NAME=value` is accepted.
- A value in matching single or double quotes loses them. There is no variable expansion and
  no escape sequences, so `$` and `#` in a value are kept as written.
- Names that are not secret inputs of the script are ignored, so one file can serve several scripts.
- A line that isn't `NAME=value` stops the run, and the message names the line number
  but doesn't repeat the line.

```bash
# a file (mode 0600), which can be given more than once
weir run s3_sync.weir --secrets-file ./aws.env

# typed at a prompt, one question for each secret, with echo off
weir run s3_sync.weir --secrets-stdin

# fed from another program
pass show aws/env | weir run s3_sync.weir --secrets-stdin

# from the environment
WEIR_SECRET_TOKEN="$DEPLOY_TOKEN" weir run deploy.weir
```

- **A file** must not be readable by group or others. Weir refuses a file with
  looser permissions than mode 0600 and tells you to `chmod 600` it. When several files are
  given, the first one that has a name wins.
- **Stdin on a terminal** prompts for each secret that no file gave, in the order the script
  declares them, and doesn't echo what you type. Pressing Enter with nothing typed uses the
  environment variable `WEIR_SECRET_<NAME>` if it is set, and the prompt says so. If it isn't set,
  the secret is undefined and the run stops, except for an optional secret (`secret?`), which is
  then left out.
- **Stdin that is not a terminal** is read to the end as a feed of `NAME=value` lines, for as
  many secrets as it holds. A secret it doesn't name falls back to the environment.
- **The environment** is the easiest in CI (see [Using Weir in CI](ci.md)). Writing the
  variable in front of the command in an interactive shell puts the value in your shell history,
  so type it that way only in a script, or use a file or the prompt.

If none of the three has a value, Weir stops with return code 2 before the first step runs, and the
message names all three. `--access_key_id value` is also return code 2, with the same message.

## What a script can do with it

A secret can't be printed, put into a string, shown in a failure message, written to a file,
or passed in a program's argument list. `weir check` reports a use it can see
(`E-SECRET-LEAK`), and the interpreter refuses at run time what the checker can't see.
It is redacted in logs and in `--plan` output.

A secret can reach a program in two ways. The first is the tool's `env` block:

```weir
tool aws_sync {
  program "aws"
  in access_key_id: secret
  in secret_access_key: secret
  env {
    AWS_ACCESS_KEY_ID = access_key_id
    AWS_SECRET_ACCESS_KEY = secret_access_key
  }
  args ["s3", "sync", "/source", "s3://bucket"]
  ...
}
```

The second is the tool's standard input (`takes text`), for a program that reads a password from stdin.

Weir starts every program with an empty environment plus `PATH`, `HOME` and what the
tool's `env` block names. The program doesn't inherit `WEIR_SECRET_*` or anything else from
your shell. [`s3_sync.weir`](examples.md) and `pg_backup.weir` are complete examples.

## Where it can still be seen

Keeping a secret out of the argument list keeps it out of `ps`. It doesn't hide it from everything:

- **The environment of a process.** Weir, and the program the secret reaches through `env`,
  have it in their environment. The same user and root can read it from `/proc/<pid>/environ`, and
  `ps e` shows it for your own processes.
- **Programs that print it.** A tool that echoes its environment or its credentials into its own
  output will put them in the result. Weir can't redact what a program chooses to print.
- **Programs that only take it on the command line.** Some programs accept a credential only as an
  argument. The argument list is public, so Weir can't wrap such a program safely. Pass the secret
  some other way, such as a config file the program reads, or choose another program.

See also [Command line](cli.md#run-options) for the options, [the design](DESIGN.md#112-input-flags)
for the rules, and [Implementation status](IMPLEMENTATION.md) for what the checker doesn't yet
follow (a secret passed through a `fn` return value or a record field).
