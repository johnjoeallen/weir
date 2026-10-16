# Examples

The scripts are in the `examples/` folder and are installed under
`/usr/share/doc/weir/examples/` by the Debian package.

| Script | What it does |
|---|---|
| `commands.weir` | Every way Weir starts a program: declared tools, chaining, the generic `run`, `ok_codes`, `try` and `catch`. See [Running programs](commands.md). Read-only. |
| `dpkgwatch.weir` | Summarises package changes from `dpkg.log`: counts by action, most-changed packages, busiest days, and removed packages. Read-only. |
| `logwatch.weir` | Summarises an nginx access log and can block abusive clients by editing a deny file, with `confirmable` steps, a lock, and a rollback if `nginx -t` rejects the change. |
| `parse_access.weir` | A local component imported by `logwatch.weir`: parses nginx combined-format lines into typed records with pure date arithmetic, so no program is started per line. |
| `s3_sync.weir` | Syncs a folder to an S3 bucket with `aws`. The keys are `secret` inputs that reach `aws` only through its environment, and `aws` sees the source folder read-only. Shows a sandbox input declared again in the flow. A port of a 56-line shell script. |
| `pg_backup.weir` | Dumps PostgreSQL databases with daily, weekly and monthly hard-linked copies and removes old ones. Every program (`pg_dump`, `date`, `mkdir`, `ln`, `find`) is a tool. |
| `sign_release.weir` | Signs a release file with `openssl`. The key folder is an `explicit` mount that only the signing tool asks for, so the script itself and every other program cannot see the key. The smallest example of `explicit`. |
| `docker_rollout.weir` | Replaces a Docker Compose service without downtime and rolls back if the new containers are not healthy. The Docker socket is an `explicit` mount that only the two tools needing it can reach. |

```bash
./examples/dpkgwatch.weir --top 5
weir test examples/dpkgwatch.weir
weir run examples/logwatch.weir block --log /var/log/nginx/access.log --plan
```

The [design document](DESIGN.md) contains two longer worked examples, a log
analysis with a config change and a certificate expiry checker.
