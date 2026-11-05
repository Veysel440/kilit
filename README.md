<div align="center">

# kilit

**Know what MySQL will do with your migration before you deploy it.**

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange?logo=rust)](https://www.rust-lang.org)
[![unsafe forbidden](https://img.shields.io/badge/unsafe-forbidden-success)](Cargo.toml)
[![MySQL 8.0 and 8.4](https://img.shields.io/badge/MySQL-8.0%20%7C%208.4-4479A1?logo=mysql&logoColor=white)](#what-mysql-does-with-common-changes)
[![Status: early development](https://img.shields.io/badge/status-early%20development-yellow)](#roadmap)

[Why](#why) · [Example](#example) · [Findings](#what-mysql-does-with-common-changes) · [Getting started](#getting-started) · [In CI](#in-ci) · [How it works](#how-it-works) · [Roadmap](#roadmap)

</div>

kilit reads a schema migration and tells you, statement by statement, what
MySQL will do with it: change only the table's metadata, work on the table in
place, or copy every row into a new table, and whether writes keep flowing
while it runs. It reads plain SQL or the output of `php artisan migrate
--pretend`. Give it your schema as well and it follows your tables through the
migration, so it can judge `->change()` too. Every answer comes from
measurements on real MySQL servers, not from guesses, and the exit status can
stop a deploy that would make writes wait.

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

Six pending migrations of a small Laravel app, piped from `migrate --pretend`
into kilit with the app's schema. Four of them:

```text
$ php artisan migrate --pretend | kilit check --schema database/schema/mysql-schema.sql
checked against mysql-8.4.13, as measured by kilit-lab
tables from database/schema/mysql-schema.sql: 4 (migrations, orders, shops, users)

2026_11_02_090100_widen_amount_on_orders_table
  alter table `orders` modify `amount` bigint not null
    COPY, copies every row, writes wait
    measured as change-integer-type (3 lab scenarios)
    - changes the type from INT to BIGINT
    - if a value does not fit the new type (in strict SQL mode, the default): fails with error 1264 (int-to-tinyint-out-of-range)

2026_11_02_090200_add_shop_to_orders_table
  alter table `orders` add `shop_id` bigint unsigned null
    INSTANT, no rebuild, writes continue
    measured as add-column (7 lab scenarios)
    - if the table has had 64 instant column changes since it was last rebuilt: INPLACE, rebuilds the table, writes continue (add-column-after-64-changes)
  alter table `orders` add constraint `orders_shop_id_foreign` foreign key (`shop_id`) references `shops` (`id`)
    COPY, copies every row, writes wait
    measured as add-foreign-key (1 lab scenario)
    - assumes foreign_key_checks is ON, the MySQL default: nothing before this statement sets it
    - if a row points to a parent row that does not exist: fails with error 1452 (add-foreign-key-orphans)

2026_11_02_090300_create_order_events_table
  create table `order_events` (`id` bigint unsigned not null auto_increment primary key, `order_id`...
    not checked: creates a new table; no existing rows are touched
  alter table `order_events` add constraint `order_events_order_id_foreign` foreign key (`order_id`) references `orders` (`id`) on delete cascade
    COPY, copies every row, writes wait
    measured as add-foreign-key (1 lab scenario)
    - assumes foreign_key_checks is ON, the MySQL default: nothing before this statement sets it
    - if a row points to a parent row that does not exist: fails with error 1452 (add-foreign-key-orphans)
    - order_events is created earlier in these migrations: nothing else uses it yet, so nothing waits for it

2026_11_02_090400_lengthen_status_on_orders_table
  alter table `orders` modify `status` varchar(30) not null
    UNKNOWN: 2 changes in one statement. kilit has measured them one at a time, and MySQL picks one algorithm for the whole statement, which can be heavier than each change alone:
      - changes VARCHAR(20) to VARCHAR(30), 80 to 120 bytes in utf8mb4
      - drops the default 'new'

metadata lock on orders:
  Every ALTER TABLE first waits for the open transactions that have used its
  table to end. While it waits, new queries on that table queue behind it,
  reads too, even for an INSTANT change, and by default MySQL lets it wait a
  year (lock_wait_timeout = 31536000). Run SET SESSION lock_wait_timeout = 5
  before the migration to cap the wait: the ALTER then fails with error 1205
  after 5 seconds, the queries behind it go on, and you can run the migration
  again later.
  measured by kilit-lab on mysql-8.4.13

8 statements: 3 make writes wait, 1 unknown, 2 run while writes continue, 1 changes a table created here, 1 not checked
```

- `$table->bigInteger('amount')->change()` copies the table, and writes wait
  until it is done.
- `$table->foreignId('shop_id')->nullable()->constrained()` is two
  statements: the column is instant, the foreign key copies the table.
- The foreign key of the new `order_events` table copies too, but that table
  has no rows and nothing uses it yet, so kilit does not count it against
  you.
- `$table->string('status', 30)->change()` looks like one change. Since
  Laravel 11, `->change()` replaces the whole column definition, so the
  `->default('new')` it does not repeat is dropped as well. kilit has measured
  each change alone, not the two together, and says so instead of guessing.
- Every `ALTER TABLE` waits for transactions that are using its table, and
  holds up the queries that come after it. kilit-lab measures this on every
  server, and kilit warns about it once, naming the tables.

kilit exits with 1 here, because three statements make writes wait.

Every statement gets one of three answers:

- **A measured verdict:** the algorithm MySQL picks, whether it rebuilds or
  copies the table, and whether writes continue, with the lab scenarios behind
  it. Notes say what kilit worked out, and list measured situations that
  change the answer but that no schema shows, such as the rows in the table.
- **Unknown,** with the reason. kilit never reports a change it has not
  measured as safe.
- **Not checked,** for statements that are not schema changes, such as
  `INSERT` or `SET`. Session settings like `foreign_key_checks` are still
  followed into the statements after them.

## What MySQL does with common changes

Measured with kilit-lab on MySQL 8.0.46 and 8.4.13, on a table of 1,000 rows.
Both versions behave the same in every scenario.

### Columns, indexes and foreign keys

| Change | MySQL does | Writes |
| --- | --- | --- |
| Add a column¹<br>`$table->string('coupon')->nullable()` | INSTANT | continue |
| ↳ when the table has a FULLTEXT index | COPY, copies every row | **wait** |
| ↳ after 64 instant column changes | INPLACE, rebuilds the table | continue |
| ↳ with `DEFAULT (UUID())` or `DEFAULT (RAND())` | fails, error 1674 (binary log on) | – |
| ↳ with `DEFAULT (NOW())`, `(CURRENT_TIMESTAMP)`, `(CURDATE())`, `(CURRENT_DATE + INTERVAL 1 YEAR)` or `(JSON_ARRAY())`<br>`$table->date('due_on')->useCurrent()` | COPY, copies every row | **wait** |
| Drop a column<br>`$table->dropColumn('note')` | INSTANT | continue |
| ↳ when the table has a FULLTEXT index | COPY, copies every row | **wait** |
| ↳ when the column is in an index, alone or with others, or has a UNIQUE index of its own, and the table has no FULLTEXT index<br>`$table->index('amount')` | INPLACE, rebuilds the table | continue |
| ↳ when the column is the only column of the only FULLTEXT index, which goes with it | INPLACE, rebuilds the table | continue |
| ↳ when the column is in a foreign key<br>`$table->dropColumn('user_id')` before `$table->dropForeign(['user_id'])` | fails, error 1828 | – |
| ↳ when the column is in a foreign key of several columns or with the checks off, the primary key, a functional index or a UNIQUE index with other columns | not answered yet | – |
| ↳ when a generated column uses it | fails, error 3108 | – |
| ↳ when a CHECK constraint uses it and another column | fails, error 3959 | – |
| Rename a column<br>`$table->renameColumn('note', 'memo')` | INSTANT | continue |
| ↳ when a generated column uses it | fails, error 3108 | – |
| ↳ when a CHECK constraint uses it, with other columns or alone | fails, error 3959 | – |
| Set a column default<br>`ALTER COLUMN … SET DEFAULT` (raw SQL) | INSTANT | continue |
| Add an index<br>`$table->index('amount')` | INPLACE | continue |
| ↳ on `LOWER()` of a VARCHAR column (functional index)<br>`$table->rawIndex('(lower(email))', 'users_lower_email_index')` | INPLACE | **wait** |
| ↳ on a TEXT or BLOB column without a prefix length<br>`$table->text('body')->index()` | fails, error 1170 | – |
| Add a unique index<br>`$table->unique('email')` | INPLACE | continue |
| ↳ when values repeat | fails, error 1062 | – |
| ↳ on a TEXT or BLOB column without a prefix length<br>`$table->text('body')->unique()` | fails, error 1170 | – |
| Add a FULLTEXT index<br>`$table->fullText('note')` | INPLACE, rebuilds the table | **wait** |
| ↳ when the table already has one | INPLACE | **wait** |
| Drop an index<br>`$table->dropIndex('orders_amount_index')` | INPLACE | continue |
| ↳ when a foreign key needs it<br>`$table->dropIndex('orders_user_id_foreign')` before `$table->dropForeign(['user_id'])` | fails, error 1553 | – |
| Add a foreign key<br>`$table->foreign('user_id')->references('id')->on('users')` | COPY, copies every row | **wait** |
| ↳ when a row points to a missing parent | fails, error 1452 | – |
| ↳ with the checks off²<br>`Schema::disableForeignKeyConstraints()` | INPLACE | continue |
| Drop a foreign key<br>`$table->dropForeign(['user_id'])` | INPLACE | continue |
| Add a CHECK constraint<br>`ADD CONSTRAINT … CHECK (…)` (raw SQL) | COPY, copies every row | **wait** |
| ↳ when a row breaks it | fails, error 3819 | – |
| ↳ when its expression has a string, a function call, `IN`, `BETWEEN` or `LIKE`, or it is `NOT ENFORCED` | not answered yet | – |
| Drop a CHECK constraint<br>`DROP CONSTRAINT …` (raw SQL) | INSTANT | continue |
| ↳ with `DROP CHECK …`, which kilit cannot read yet | not answered yet | – |

¹ Seven ways of adding a column were measured, and all are INSTANT: nullable,
`NOT NULL` with and without a default, `AFTER`, `FIRST`, with a character set,
collation and comment, and a `TIMESTAMP` defaulting to `CURRENT_TIMESTAMP`.\
² Even when a row points to a missing parent: the foreign key is added, and the
row stays as it is, unchecked.

### Changing a column with `->change()`

Laravel turns `->change()` into `MODIFY`, which replaces the column's whole
definition. kilit compares it with the column's current definition, from your
schema, and answers for what actually changes.

| Change | MySQL does | Writes |
| --- | --- | --- |
| Change between integer types³<br>`$table->bigInteger('amount')->change()` | COPY, copies every row | **wait** |
| ↳ when the column is in a foreign key | fails, error 3780 | – |
| ↳ when a value does not fit the new type | fails, error 1264 | – |
| Lengthen a VARCHAR within 255 bytes⁴<br>`$table->string('note', 63)->nullable()->change()` | INPLACE | continue |
| Lengthen a VARCHAR past 255 bytes⁴<br>`$table->string('note', 64)->nullable()->change()` | COPY, copies every row | **wait** |
| Shorten a VARCHAR<br>`$table->string('note', 40)->nullable()->change()` | COPY, copies every row | **wait** |
| ↳ when a value is longer than the new length | fails, error 1265 | – |
| Change the character set⁵<br>`$table->string('note', 50)->charset('latin1')->nullable()->change()` | COPY, copies every row | **wait** |
| Change the collation<br>`$table->string('note', 50)->collation('utf8mb4_bin')->nullable()->change()` | INPLACE | continue |
| ↳ when the column is in an index | COPY, copies every row | **wait** |
| Make a column nullable<br>`$table->integer('amount')->nullable()->change()` | INPLACE, rebuilds the table | continue |
| Make a column NOT NULL<br>`$table->string('email')->nullable(false)->change()` | INPLACE, rebuilds the table | continue |
| ↳ when a row holds NULL | fails, error 1138 | – |
| Give, change or drop a default<br>`$table->integer('amount')->default(0)->change()` | INSTANT | continue |
| Move a column<br>`$table->string('note', 50)->nullable()->after('user_id')->change()` | INPLACE, rebuilds the table | continue |
| Rename a column with `CHANGE`<br>`$table->renameColumn()` before MySQL 8.0.3 | INSTANT | continue |
| Keep the same definition<br>`->change()` with nothing new | INSTANT | continue |

³ `INT` to `BIGINT`, to `SMALLINT` and to `INT UNSIGNED`.\
⁴ In `utf8mb4` a character takes up to 4 bytes, so `VARCHAR(63)` is 252 bytes
and `VARCHAR(64)` is 256; in `latin1`, one byte each. Past 255 bytes, MySQL
stores each value's length in 2 bytes instead of 1. Both edges were measured
in both character sets.\
⁵ Including a `MODIFY` that names no character set on a column whose set
differs from the table's: the column takes the table's, and MySQL copies the
table to convert it.

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
cargo install --locked --path crates/kilit   # puts kilit in ~/.cargo/bin
```

In a Laravel project:

```sh
php artisan schema:dump
php artisan migrate --pretend | kilit check --schema database/schema/mysql-schema.sql
```

`schema:dump` writes your tables to `database/schema/mysql-schema.sql`, with
the `mysqldump` client. Laravel loads that file before it runs migrations on an
empty database; if you do not want that, write the file elsewhere with
`--path`. Run both commands against a database whose tables match production,
because that is where the migration will run. Without `--schema`, kilit still
answers everything that does not depend on the table, and says so where it
does.

For SQL files, or saved `--pretend` output:

```sh
kilit check --schema schema.sql migration.sql   # several files run in order, as one migration
kilit check --server mysql-8.0.46 migration.sql # the default is the newest server, mysql-8.4.13
kilit servers                                   # the servers kilit has results for
```

On Windows, pipe in WSL, Git Bash or `cmd`, which pass Laravel's output
through as it is. PowerShell before 7.4 re-encodes text in a pipe and with `>`:
kilit still reads the statement mark that this garbles, but text in other
languages inside your SQL can come through garbled.

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

## In CI

`kilit check` exits with:

| Status | When |
| --- | --- |
| 0 | no statement fails and none makes writes wait |
| 1 | a statement fails or makes writes wait; with `--strict`, also when kilit cannot tell what a statement does |
| 2 | kilit could not check everything: a bad option, a file it cannot read, an unknown server, or lines of the input it does not understand |

`--format json` prints the same answers as one JSON document: the migrations
with their statements, each with its `impact` (`fails`, `writes_wait`,
`writes_continue`, `new_table`, `unknown` or `not_checked`), the measured
algorithm and the lab scenarios behind it, the warnings, a summary and the exit
status.

## How it works

```mermaid
flowchart LR
    scenarios["scenarios/*.toml<br/>one change each"] --> lab["kilit-lab"]
    lab <-->|"6 runs per scenario"| servers[("MySQL 8.0.46<br/>MySQL 8.4.13")]
    lab --> matrix["matrix/*.json<br/>what each server did"]
    matrix -->|"compiled in"| core["kilit-core"]
    migration["your migration<br/>SQL or migrate --pretend"] --> cli["kilit check"]
    schema["your schema"] --> cli
    core --> cli
    cli --> answers["an answer per statement<br/>and an exit status"]
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
which scenarios changed. Before the scenarios, the lab also checks the facts
kilit uses without a scenario of their own: how many bytes a character takes in
each character set, and that an `ALTER TABLE` waits for a transaction that has
read its table and is still open, with `ALGORITHM=INSTANT`, `INPLACE` and
`COPY` alike, while a plain `SELECT` from another connection waits behind it.

**The reader** takes SQL, or the output of `php artisan migrate --pretend`,
which it recognizes on its own: Laravel 10 to 13 print it the same way, one
statement per line, under each migration's name. It splits SQL into
statements, so a semicolon inside a string or a comment does not end one, and
reads each statement with
[sqlparser](https://crates.io/crates/sqlparser)'s MySQL dialect into kilit's
own types. Nothing is dropped silently: an operation it reads but does not model
becomes `Other`, and a statement it cannot read becomes `Unparsed` without
stopping the statements after it.

**The schema** comes from `CREATE TABLE` statements, such as a schema dump.
kilit follows every statement of the migration through it: columns, indexes and
foreign keys are added, changed, renamed and dropped, so each statement is
judged against the tables as they are at that point. When a statement changes a
table in a way kilit cannot follow, such as one it cannot read, kilit stops
trusting what it knew about that table instead of guessing.

**The checker** places each change in a *case*: a kind of change, together with
the lab scenarios that measured it. For a `MODIFY` or `CHANGE`, kilit compares
the new definition with the column's current one, character set and collation
included, and the one thing that changes decides the case. The scenarios of a
case must agree on every server, or kilit refuses to answer. A case can also
have *exceptions*: the same change measured in a situation where the answer
differs. When the situation shows in the schema, such as a FULLTEXT index on the
table or a foreign key on the column, it decides the answer; when it does not,
such as rows that break a new constraint, it becomes a note. A statement that
forces `ALGORITHM=...` or `LOCK=NONE` is answered from the lab's run of exactly
that variant.

**The command** (`kilit`) reads every input as one migration, in order, so a
table created by one migration is known to the next. It sums up what each
answer means while the migration runs: it fails, writes wait, writes continue,
it only touches a table created earlier in the same migrations, or kilit
cannot tell. The exit status follows from that.

## Design principles

- **Measured, not guessed.** Every answer names the lab scenarios behind it.
  A golden test runs the scenarios of every case through kilit, in all six
  variants and on both servers, and fails if kilit's answer differs from what
  the server did.
- **Never silently safe.** A statement kilit cannot read, a change it has not
  measured, or several changes in one statement are reported as unknown, with
  the reason.
- **Reproducible.** Running the lab again changes a result file only when a
  server behaves differently. The lab also checks the few facts kilit uses
  without a scenario of their own, such as how many bytes a character takes
  in each character set and how `ALTER TABLE` waits for the metadata lock.
- **Pure Rust.** No C or assembly is compiled anywhere in the dependency tree,
  and `unsafe` code is forbidden in the workspace.
- **No I/O in the core.** `kilit-core` takes text and returns data. The lab's
  results are compiled into it, so checking needs no database.

## Limitations

kilit is young, and it says so when it does not know. Today:

- `MODIFY` and `CHANGE` need the table's current definition: without a
  schema, kilit lists what the lab measured instead of answering.
- A statement with several changes, including a `MODIFY` that changes several
  things about a column, is reported as unknown; the lab has measured single
  changes only.
- Type changes other than between integer types, such as `VARCHAR` to `TEXT`,
  are not measured yet.
- Only MySQL 8.0.46 and 8.4.13 are measured. MariaDB is not supported.
- sqlparser 0.63 cannot read `CONVERT TO CHARACTER SET`, `ENGINE=...`,
  `RENAME INDEX`, `ALGORITHM=NOCOPY` or `OPTIMIZE TABLE` yet, so kilit reports
  them as unreadable.
- "Writes continue" describes the change itself. Every `ALTER TABLE` also
  waits for the metadata lock first; kilit warns about it, but cannot know
  which transactions will be open when you deploy.
- Laravel's `migrate --pretend` output turns every run of spaces into one,
  even inside quotes, so kilit sees `DEFAULT 'a  b'` as `DEFAULT 'a b'`.

## Roadmap

- [x] A lab that measures schema changes on MySQL 8.0 and 8.4 and keeps
  reproducible results
- [x] A reader for MySQL migrations, including the SQL Laravel generates
- [x] Measured answers for column, index and foreign key changes, following
  session settings such as `foreign_key_checks`
- [x] Schema awareness: answers for `MODIFY` and `CHANGE` from the column's
  current definition, and definite answers for tables with a FULLTEXT index
- [x] A `kilit check` command: Laravel's `migrate --pretend` output as input,
  JSON output, and exit codes for CI
- [x] A warning about the metadata lock, measured on every server
- [ ] Continuous integration, including a scheduled lab run that catches
  behavior changes in new MySQL releases
- [ ] A first release on crates.io

## Project layout

```text
crates/kilit/        the kilit command
crates/kilit-core/   reads migrations and checks them; no I/O
crates/kilit-lab/    measures schema changes on real MySQL servers
scenarios/           one TOML file per measured change, and the lab's tables
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
