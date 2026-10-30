# kilit

kilit will tell you, before you deploy, what MySQL is going to do with a schema
migration: change it instantly, rebuild the table, or copy every row while
writes wait. Its answers will come from measurements on real servers, not from
guesses.

> **Status:** early development. The measurement lab and the SQL reader
> exist; kilit cannot check a migration yet.

The workspace has two crates:

- [`kilit-lab`](crates/kilit-lab/) measures what real servers do.
- [`kilit-core`](crates/kilit-core/) reads migrations. It does no I/O: it
  takes text and returns data.

## The lab

`kilit-lab` runs each scenario in [`scenarios/`](scenarios/) against real
MySQL servers in six ways (server default, `ALGORITHM=INSTANT|INPLACE|COPY`,
`LOCK=NONE`) and records what the server did:

- whether it accepted the statement, and the error code if it did not
- how many rows it copied (`Records: N` in the server's reply)
- whether InnoDB rebuilt the table (the table's internal id changed)
- InnoDB's counter of instant column changes (`TOTAL_ROW_VERSIONS`)

From those six runs it derives a verdict: which algorithm the server picks
when a migration forces nothing (the cheapest one it accepts, checked against
the default run), whether the table is rebuilt or copied, and whether writes
can continue while the change runs.

The results are saved to [`matrix/`](matrix/), one JSON file per server
version. They contain no timestamps or table ids, so running the lab again
only changes a file when a server behaves differently.

### Run it

You need Rust 1.88 or newer and Docker with Compose.

```sh
docker compose up -d --wait        # MySQL 8.0.46 on port 3380, 8.4.13 on port 3384
cargo run -p kilit-lab             # summary per server, results in matrix/
cargo run -p kilit-lab -- --details   # also every variant of every scenario
docker compose down                # remove the servers when you are done
```

The lab drops and recreates a database named `kilit_lab`. Only point it at
throwaway servers.

### Add a scenario

Add a `.toml` file to `scenarios/` (the format is described in
[`scenarios/README.md`](scenarios/README.md)), run `cargo test` to check that
it loads, then run the lab.

`cargo test` also checks that `kilit-core` reads every statement the lab
runs, with the same algorithm and lock. A measurement kilit cannot read back
would be useless.

## The SQL reader

`kilit-core` splits SQL text into statements (a semicolon inside a string or
a comment does not end one), reads each with
[sqlparser](https://crates.io/crates/sqlparser)'s MySQL dialect and turns it
into kilit's own types: `ALTER TABLE` operations such as `AddColumn`,
`ModifyColumn`, `AddIndex` and `AddForeignKey`, plus `SET` of session
variables like `foreign_key_checks`.

Nothing is dropped silently. An operation it reads but does not model yet
becomes `Other`; a statement it cannot read becomes `Unparsed`, and the
statements after it are still read. Neither will ever be reported as safe.

See what it reads from a file:

```sh
cargo run -p kilit-core --example parse                         # a sample migration
cargo run -p kilit-core --example parse -- path/to/migration.sql
```

Known gaps: sqlparser 0.63 cannot read `CONVERT TO CHARACTER SET`,
`ENGINE=...`, `RENAME INDEX`, MariaDB's `ALGORITHM=NOCOPY` or `OPTIMIZE TABLE`
yet, so they come out as `Unparsed`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
