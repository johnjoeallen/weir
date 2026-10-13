> # Weir
>
> Weir is a small interpreted language for scripts — anything you'd otherwise write in bash, Python, or a CI config, run from a terminal, a cron job, a systemd unit, or a pipeline.
>
> Scripts usually have a shape: inputs, a sequence of steps, some outputs. Shell languages don't express that shape, so it ends up implied by position and comments. Workflow formats like GitHub Actions do express it, but in YAML, with a stringly typed expression language wedged inside.
>
> Weir takes the shape and makes it the language.
>
> ```weir
> main deploy {
>   in  env: Env                    # a typed input; becomes --env on the CLI
>   in  version: string = run git describe --tags
>   out url: string
>
>   needs write [./target]          # what this script may touch
>
>   build(profile: env)             # a call is a step, named automatically
>
>   publish when env != dev {       # an inline step, guarded
>     url = upload(artifact: jar).url
>   }
> }
> ```
>
> - **Typed inputs become the command line.** `--help` is generated, bad values are rejected before step one.
> - **Every call is a step.** Failures read `deploy > publish`, and `--plan` lists what would run without running it.
> - **Commands take argument lists, not strings**, so quoting bugs can't be expressed.
> - **Scripts declare what they may touch**, and a component you pull in can't widen that.
> - **Components come from a local folder, GitHub, or Artifactory/Nexus**, pinned by hash.
> - **`weir check`** reports errors in terms of what you wrote, before anything executes.
>
> **Status:** early design. The language is still moving. See `docs/` for the current reference.
