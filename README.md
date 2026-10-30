<div align="center">

# kilit

**Know what MySQL will do with your migration before you deploy it.**

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange?logo=rust)](https://www.rust-lang.org)
[![unsafe forbidden](https://img.shields.io/badge/unsafe-forbidden-success)](Cargo.toml)
[![MySQL 8.0 and 8.4](https://img.shields.io/badge/MySQL-8.0%20%7C%208.4-4479A1?logo=mysql&logoColor=white)](#what-mysql-does-with-common-changes)
[![Status: early development](https://img.shields.io/badge/status-early%20development-yellow)](#roadmap)

[Why](#why) · [Example](#example) · [Findings](#what-mysql-does-with-common-changes) · [Getting started](#getting-started) · [How it works](#how-it-works) · [Roadmap](#roadmap)

</div>

kilit reads a schema migration and tells you, statement by statement, what
MySQL will do with it: change only the table's metadata, work on the table in
place, or copy every row into a new table, and whether writes keep flowing
while it runs. Every answer comes from measurements on real MySQL servers, not
from guesses.

*kilit* is Turkish for *lock*.

## Why

Some schema changes are free. Others make MySQL copy the whole table while
every write to it waits, and the migration looks the same either way. This one
line of Laravel:

```php
$table->foreignId('user_id')->constrained();
```

adds a column instantly, then copies every row of the table to add the foreign
key. On a large table, that means writes waiting for minutes in production.

MySQL documents the rules, but the answer depends on the server version, the
table and the data. A FULLTEXT index elsewhere on the table, a row that breaks
the new constraint, or a VARCHAR that grows past 255 bytes can each turn a
cheap change into a table copy or a failed deploy. kilit does not reason from
the documentation: a lab runs each kind of change against real servers and
records what they did, and kilit answers from those records.

## Example

```text
$ cargo run -p kilit-core --example check
checked against mysql-8.4.13, as measured by kilit-lab

line 4: alter table `orders` add `coupon` varchar(255) null after `amount`
  INSTANT, no rebuild, writes continue
  measured as add-column (7 lab scenarios)
  - if the table has a FULLTEXT index: COPY, copies every row, writes wait (add-column-with-fulltext)
  - if the table has had 64 instant column changes since it was last rebuilt: INPLACE, rebuilds the table, writes continue (add-column-after-64-changes)

line 7: alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`) on delete cascade
  COPY, copies every row, writes wait
  measured as add-foreign-key (1 lab scenario)
  - assumes foreign_key_checks is ON, the MySQL default: nothing before this statement sets it
  - if a row points to a parent row that does not exist: fails with error 1452 (add-foreign-key-orphans)

line 8: alter table `orders` add fulltext `orders_note_fulltext`(`note`)
  INPLACE, rebuilds the table, writes wait
  measured as add-fulltext-index (1 lab scenario)

line 10: alter table `orders` drop `legacy_code`, drop `legacy_flag`
  UNKNOWN: 2 changes in one statement. kilit has measured single changes only, and MySQL picks one algorithm for the whole statement, which can be heavier than each change alone.
```

Every statement gets one of three answers:

- **A measured verdict:** the algorithm MySQL picks, whether it rebuilds or
  copies the table, and whether writes continue, with the lab scenarios behind
  it. Notes list measured situations that change the answer and that kilit
  cannot see from the statement alone, such as the rows already in the table.
- **Unknown,** with the reason. kilit never reports a change it has not
  measured as safe.
- **Not checked,** for statements that are not schema changes, such as
  `INSERT` or `SET`. Session settings like `foreign_key_checks` are still
  followed into the statements after them.

## What MySQL does with common changes

Measured with kilit-lab on MySQL 8.0.46 and 8.4.13, on a table of 1,000 rows.
Both versions behave the same in every scenario.

| Change | MySQL does | Writes |
| --- | --- | --- |
| Add a column¹<br>`$table->string('coupon')->nullable()` | INSTANT | continue |
| ↳ when the table has a FULLTEXT index | COPY, copies every row | **wait** |
| ↳ after 64 instant column changes | INPLACE, rebuilds the table | continue |
| Drop a column<br>`$table->dropColumn('note')` | INSTANT | continue |
| ↳ when the table has a FULLTEXT index | COPY, copies every row | **wait** |
| Rename a column<br>`$table->renameColumn('note', 'memo')` | INSTANT | continue |
| Set a column default<br>`ALTER COLUMN … SET DEFAULT` (raw SQL) | INSTANT | continue |
| Add an index<br>`$table->index('amount')` | INPLACE | continue |
| Add a unique index<br>`$table->unique('email')` | INPLACE | continue |
| ↳ when values repeat | fails, error 1062 | – |
| Add a FULLTEXT index<br>`$table->fullText('note')` | INPLACE, rebuilds the table | **wait** |
| Drop an index<br>`$table->dropIndex('orders_amount_index')` | INPLACE | continue |
| Add a foreign key<br>`$table->foreign('user_id')->references('id')->on('users')` | COPY, copies every row | **wait** |
| ↳ when a row points to a missing parent | fails, error 1452 | – |
| ↳ with the checks off²<br>`Schema::disableForeignKeyConstraints()` | INPLACE | continue |
| Drop a foreign key<br>`$table->dropForeign(['user_id'])` | INPLACE | continue |
| Change INT to BIGINT<br>`$table->bigInteger('amount')->change()` | COPY, copies every row | **wait** |
| Lengthen a VARCHAR within 255 bytes³<br>`$table->string('note', 60)->nullable()->change()` | INPLACE | continue |
| Lengthen a VARCHAR past 255 bytes³<br>`$table->string('note', 100)->nullable()->change()` | COPY, copies every row | **wait** |
| Make a column NOT NULL<br>`$table->string('email')->nullable(false)->change()` | INPLACE, rebuilds the table | continue |
| ↳ when a row holds NULL | fails, error 1138 | – |

¹ Seven ways of adding a column were measured, and all are INSTANT: nullable,
`NOT NULL` with and without a default, `AFTER`, `FIRST`, with a character set,
collation and comment, and a `TIMESTAMP` defaulting to `CURRENT_TIMESTAMP`.\
² Even when a row points to a missing parent: the foreign key is added, and the
row stays as it is, unchecked.\
³ The column is `utf8mb4`, where a character takes up to 4 bytes: `VARCHAR(50)`
is 200 bytes, `VARCHAR(60)` is 240 and `VARCHAR(100)` is 400. Past 255 bytes,
MySQL stores each value's length in 2 bytes instead of 1.

INSTANT changes only the table's metadata. INPLACE works on the existing
table; where it rebuilds the table, it rewrites every row, which takes time on
a large table even while writes continue. COPY builds a new table and copies
every row into it. Writes **wait** when the server refuses `LOCK=NONE` for the
change: inserts, updates and deletes on the table wait until it finishes.

Every row comes from a scenario in [`scenarios/`](scenarios/) and its results
in [`matrix/`](matrix/).

## Getting started

You need [Rust](https://rustup.rs) 1.88 or newer. Checking a migration needs no
database: the lab's results are compiled into kilit.

```sh
git clone https://github.com/Veysel440/kilit.git
cd kilit

# the sample migration in crates/kilit-core/examples/sample.sql
cargo run -p kilit-core --example check

# your own SQL, against MySQL 8.4.13 (the default) or 8.0.46
cargo run -p kilit-core --example check -- path/to/migration.sql
cargo run -p kilit-core --example check -- path/to/migration.sql mysql-8.0.46
```

kilit reads plain SQL for now. In a Laravel project, `php artisan migrate
--pretend` prints the statements your migrations will run; save them to a
file. Reading that output directly is on the [roadmap](#roadmap).

### Running the lab

The lab needs Docker with Compose.

```sh
docker compose up -d --wait          # MySQL 8.0.46 on port 3380, 8.4.13 on port 3384
cargo run -p kilit-lab               # a summary per server; results go to matrix/
cargo run -p kilit-lab -- --details  # also every variant of every scenario
docker compose down                  # remove the servers
```

> [!WARNING]
> The lab drops and recreates a database named `kilit_lab`. Only point it at
> throwaway servers.

## How it works

```mermaid
flowchart LR
    scenarios["scenarios/*.toml<br/>one change each"] --> lab["kilit-lab"]
    lab <-->|"6 runs per scenario"| servers[("MySQL 8.0.46<br/>MySQL 8.4.13")]
    lab --> matrix["matrix/*.json<br/>what each server did"]
    matrix -->|"compiled in"| core["kilit-core"]
    migration["your migration"] --> core
    core --> answers["an answer per statement"]
```

**The lab** (`kilit-lab`) starts every scenario from the same two tables,
`users` and `orders`, and runs its change six ways: as written, then with
`ALGORITHM=INSTANT`, `ALGORITHM=INPLACE`, `ALGORITHM=INPLACE, LOCK=NONE`,
`ALGORITHM=COPY` and `LOCK=NONE`. For each run it records whether the server
accepted the statement or which error it returned, how many rows it copied,
whether InnoDB rebuilt the table, and InnoDB's count of instant column changes.
From the six runs it derives a verdict: the algorithm the server picks when
nothing is forced, checked against the run as written, whether the table is
rebuilt or copied, and whether writes can continue. The results go to
`matrix/`, one JSON file per server version. They hold no timestamps or ids, so
a file changes only when a server behaves differently, and the lab reports
which scenarios changed.

**The reader** splits SQL into statements, so a semicolon inside a string or a
comment does not end one, and reads each statement with
[sqlparser](https://crates.io/crates/sqlparser)'s MySQL dialect into kilit's
own types. Nothing is dropped silently: an operation it reads but does not model
becomes `Other`, and a statement it cannot read becomes `Unparsed` without
stopping the statements after it.

**The checker** places each change in a *case*: a kind of change, together with
the lab scenarios that measured it. The scenarios of a case must agree on every
server, or kilit refuses to answer. A case can also have *exceptions*: the same
change measured in a situation where the answer differs, such as a table with a
FULLTEXT index or rows that break a new constraint. kilit cannot see the table
or its rows yet, so it reports exceptions as notes. A statement that forces
`ALGORITHM=...` or `LOCK=NONE` is answered from the lab's run of exactly that
variant.

## Design principles

- **Measured, not guessed.** Every answer names the lab scenarios behind it.
  A golden test runs the scenarios of every case through kilit, in all six
  variants and on both servers, and fails if kilit's answer differs from what
  the server did.
- **Never silently safe.** A statement kilit cannot read, a change it has not
  measured, or several changes in one statement are reported as unknown, with
  the reason.
- **Reproducible.** Running the lab again changes a result file only when a
  server behaves differently.
- **Pure Rust.** No C or assembly is compiled anywhere in the dependency tree,
  and `unsafe` code is forbidden in the workspace.
- **No I/O in the core.** `kilit-core` takes text and returns data. The lab's
  results are compiled into it, so checking needs no database.

## Limitations

kilit is young, and it says so when it does not know. Today:

- `MODIFY` and `CHANGE` are reported as unknown, because kilit does not read
  the table's current definition yet. It lists what the lab measured instead.
- A statement with several changes is reported as unknown; the lab has
  measured single changes only.
- Only MySQL 8.0.46 and 8.4.13 are measured. MariaDB is not supported.
- sqlparser 0.63 cannot read `CONVERT TO CHARACTER SET`, `ENGINE=...`,
  `RENAME INDEX`, `ALGORITHM=NOCOPY` or `OPTIMIZE TABLE` yet, so kilit reports
  them as unreadable.
- "Writes continue" describes the change itself. Every `ALTER TABLE`, even an
  instant one, briefly needs an exclusive metadata lock on the table. If a long
  transaction is using the table, the `ALTER` waits for it, and new queries on
  the table queue up behind the `ALTER`. kilit does not warn about this yet.

## Roadmap

- [x] A lab that measures schema changes on MySQL 8.0 and 8.4 and keeps
  reproducible results
- [x] A reader for MySQL migrations, including the SQL Laravel generates
- [x] Measured answers for column, index and foreign key changes, following
  session settings such as `foreign_key_checks`
- [ ] Schema awareness: answers for `MODIFY` and `CHANGE` from the column's
  current definition, and definite answers for tables with a FULLTEXT index
- [ ] A `kilit check` command: Laravel's `migrate --pretend` output as input,
  JSON output, and exit codes for CI
- [ ] Continuous integration, including a scheduled lab run that catches
  behavior changes in new MySQL releases
- [ ] A first release on crates.io

## Project layout

```text
crates/kilit-core/   reads migrations and checks them; no I/O
crates/kilit-lab/    measures schema changes on real MySQL servers
scenarios/           one TOML file per measured change
matrix/              the lab's results, one JSON file per server version
compose.yaml         the MySQL servers the lab runs against
```

## Contributing

The most valuable contribution is a new measurement. A scenario is a small TOML
file in [`scenarios/`](scenarios/):

```toml
title = "Add a FULLTEXT index"
operation = "ADD FULLTEXT INDEX ft_orders_note (note)"
```

[`scenarios/README.md`](scenarios/README.md) explains the format and the three
steps from a new file to an answer kilit gives. Before you open a pull request,
run:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
