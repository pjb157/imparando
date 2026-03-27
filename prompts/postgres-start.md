PostgreSQL is expected to run locally inside this VM on `127.0.0.1:5432`.

If the app or tests fail because Postgres is not running, use this recovery flow.

Rules:

1. Do not use `systemctl`. These VMs are not booted with systemd.
2. Do not install another database package unless you have confirmed PostgreSQL is actually missing.
3. Prefer the built-in helper first.

Basic checks:

```sh
pg_isready -h 127.0.0.1 -p 5432
ps aux | grep postgres
cat /proc/swaps
```

First recovery attempt:

```sh
/usr/local/bin/start-postgres.sh
pg_isready -h 127.0.0.1 -p 5432
```

If that fails, inspect the log:

```sh
cat /var/log/postgresql/postgresql.log
```

If there is no cluster yet, verify what PostgreSQL versions are installed:

```sh
ls /usr/lib/postgresql
```

Then retry the built-in helper. It is responsible for initializing the data directory and starting PostgreSQL for the installed version.

Important constraints:

- PostgreSQL must not be started as root directly with `initdb` or `postgres`.
- The helper handles running the server under the `postgres` user.
- Keep the server bound to `127.0.0.1` on port `5432`.

If startup still fails after checking the log, report:

- output of `pg_isready -h 127.0.0.1 -p 5432`
- output of `ls /usr/lib/postgresql`
- contents of `/var/log/postgresql/postgresql.log`

Do not invent a different database setup without those checks.
