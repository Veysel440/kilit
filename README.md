# kilit

kilit will tell you, before you deploy, what MySQL is going to do with a schema
migration: change it instantly, rebuild the table, or copy every row while
writes wait. Its answers will come from measurements on real servers, not from
guesses.

> **Status:** early development. The measurement lab, the SQL reader and a
> first checker exist; there is no command-line tool yet.

The workspace has two crates:

- [`kilit-lab`](crates/kilit-lab/) measures what real servers do.
- [`kilit-core`](crates/kilit-core/) reads migrations and checks them
  against the lab's results. It does no I/O: it takes text and returns data.

## What the lab found

Measured on MySQL 8.0.46 and 8.4.13 (the results are the same on both):

| Change | Laravel | What MySQL does |
| ------ | ------- | --------------- |
| Add a column | `$table->string('coupon')->nullable()`, `->after()`, `->default()` | INSTANT, no rebuild, writes continue |
| Add a column to a table with a FULLTEXT index | the same | COPY, copies every row, writes wait |
| Add a foreign key | `->constrained()` | COPY, copies every row, writes wait |
| Add a foreign key with the checks off | inside `Schema::disableForeignKeyConstraints()` | INPLACE, no rebuild, writes continue |
| Add a FULLTEXT index | `$table->fullText('note')` | INPLACE, rebuilds the table, writes wait |
| Add an index or a unique index | `$table->index('amount')`, `->unique()` | INPLACE, no rebuild, writes continue |
| Change a type, INT to BIGINT | `$table->bigInteger('amount')->change()` | COPY, copies every row, writes wait |

Every line comes from a scenario in [`scenarios/`](scenarios/) and its
results in [`matrix/`](matrix/). Tests check that kilit says exactly this.

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
only changes a file when a server behaves differently. Before saving, the lab
compares its results with the saved file and reports which scenarios are new
or changed.

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

Add a `.toml` file to `scenarios/`; the format and the steps are in
[`scenarios/README.md`](scenarios/README.md).

`cargo test` checks that `kilit-core` reads every statement the lab runs,
with the same algorithm and lock, and that every measured scenario is used
by kilit. A measurement kilit cannot read back, or never uses, helps no one.

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

## Checking a migration

kilit places each change in a *case*: a kind of change, such as `add-column`,
together with the lab scenarios that measured it. The lab results are built
into kilit-core when it is compiled, so checking needs no database.

- **Measured:** kilit gives the server's answer for the target version, and
  names the scenarios behind it. The scenarios of a case must agree, or kilit
  refuses to answer.
- **Notes:** the same change measured on a table or on data where the answer
  is different, such as a table with a FULLTEXT index or a foreign key with
  rows that point to a missing parent. kilit cannot see the table or the data
  yet, so it lists these instead of guessing.
- **Unknown:** changes the lab has not measured, several changes in one
  statement, `MODIFY` (it needs the column's current definition) and
  statements kilit could not read. kilit says why, and never calls them safe.

`SET foreign_key_checks = 0` and similar session settings are followed from
statement to statement. A statement that forces `ALGORITHM=...` or
`LOCK=NONE` is answered from the lab's run of exactly that variant.

```sh
cargo run -p kilit-core --example check                                   # a sample migration
cargo run -p kilit-core --example check -- path/to/migration.sql mysql-8.0.46
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
