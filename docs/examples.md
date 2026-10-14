# Examples

The scripts are in the `examples/` folder and are installed under
`/usr/share/doc/weir/examples/` by the Debian package.

| Script | What it does |
|---|---|
| `dpkgwatch.weir` | Summarises package changes from `dpkg.log`: counts by action, most-changed packages, busiest days, and removed packages. Read-only. |
| `logwatch.weir` | Summarises an nginx access log and can block abusive clients by editing a deny file, with `confirmable` steps, a lock, and a rollback if `nginx -t` rejects the change. |
| `parse_access.weir` | A local component imported by `logwatch.weir`: parses nginx combined-format lines into typed records with pure date arithmetic, so no program is started per line. |

```bash
./examples/dpkgwatch.weir --top 5
weir test examples/dpkgwatch.weir
weir run examples/logwatch.weir block --log /var/log/nginx/access.log --plan
```

The [design document](DESIGN.md) contains two longer worked examples, a log
analysis with a config change and a certificate expiry checker.
