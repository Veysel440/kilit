<div align="center">

# kilit

**Know what MySQL will do with your migration before you deploy it.**

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange?logo=rust)](https://www.rust-lang.org)
[![unsafe forbidden](https://img.shields.io/badge/unsafe-forbidden-success)](Cargo.toml)
[![MySQL 8.0 and 8.4](https://img.shields.io/badge/MySQL-8.0%20%7C%208.4-4479A1?logo=mysql&logoColor=white)](#what-mysql-does-with-common-changes)
[![Status: early development](https://img.shields.io/badge/status-early%20development-yellow)](#roadmap)

[Why](#why) · [Example](#example) · [Findings](#what-mysql-does-with-common-changes) · [Getting started](#getting-started) · [In CI](#in-ci) · [JSON output](#json-output) · [How it works](#how-it-works) · [Roadmap](#roadmap)

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
    measured as add-foreign-key (16 lab scenarios)
    - assumes foreign_key_checks is ON, the MySQL default: nothing before this statement sets it
    - if a row points to a parent row that does not exist: fails with error 1452 (add-foreign-key-orphans)

2026_11_02_090300_create_order_events_table
  create table `order_events` (`id` bigint unsigned not null auto_increment primary key, `order_id`...
    not checked: creates a new table; no existing rows are touched
  alter table `order_events` add constraint `order_events_order_id_foreign` foreign key (`order_id`) references `orders` (`id`) on delete cascade
    COPY, copies every row, writes wait
    measured as add-foreign-key (16 lab scenarios)
    - assumes foreign_key_checks is ON, the MySQL default: nothing before this statement sets it
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

Measured with kilit-lab on MySQL 8.0.46 and 8.4.13, on InnoDB tables of 1,000
rows. A table whose `CREATE TABLE` names no `ENGINE` is taken as InnoDB, the
server default; [MyISAM tables](#myisam-tables) have their own rows below. The
two versions mostly behave the same. Where they differ, the table says which
one does what.

### Columns, indexes and foreign keys

| Change | MySQL does | Writes |
| --- | --- | --- |
| Add a column¹<br>`$table->string('coupon')->nullable()` | INSTANT | continue |
| ↳ when the table has a FULLTEXT index | COPY, copies every row | **wait** |
| ↳ after 64 instant column changes | INPLACE, rebuilds the table | continue |
| ↳ with `DEFAULT (UUID())`, `(RAND())`, `(UUID_SHORT())`, `(SYSDATE())` or `(USER())` | fails, error 1674 (binary log on) | – |
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
| ↳ when the parent column is BIGINT UNSIGNED and the column is an INT, INT UNSIGNED, BIGINT or smaller unsigned integer, or the parent is INT UNSIGNED and the column BIGINT UNSIGNED | fails, error 3780 | – |
| ↳ when the parent column is utf8mb4 with its default collation and the column is a VARCHAR of the same length in latin1 or utf8mb4_bin | fails, error 3780 | – |
| ↳ when no index of the parent table starts with the parent column, the column is only second in an index, or the index covers only a shorter prefix such as `name(10)` | fails, error 1822 on 8.0.46, 6125 on 8.4.13 | – |
| ↳ when the only index that starts with the parent column is a plain one, a plain DESC one, or a longer one | COPY, copies every row on 8.0.46; fails, error 6125 on 8.4.13 | **wait** on 8.0.46, – on 8.4.13 |
| ↳ when the two columns differ in a way not measured, the key has several columns without a matching UNIQUE index or primary key, or the parent index has a prefix as long as the column or longer, is FULLTEXT only, or part of a longer primary key, or the parent column has a foreign key of its own | not answered yet | – |
| ↳ with the checks off²<br>`Schema::disableForeignKeyConstraints()` | INPLACE | continue |
| ↳ with the checks off, when the two columns visibly differ, the parent has no fitting index, the key points at its own table or the shape is not measured | not answered yet | – |
| Drop a foreign key<br>`$table->dropForeign(['user_id'])` | INPLACE | continue |
| Add a CHECK constraint<br>`ADD CONSTRAINT … CHECK (…)` (raw SQL) | COPY, copies every row | **wait** |
| ↳ when a row breaks it | fails, error 3819 | – |
| ↳ when its expression has a string, a function call, `IN`, `BETWEEN` or `LIKE`, or it is `NOT ENFORCED` | not answered yet | – |
| Drop a CHECK constraint<br>`DROP CONSTRAINT …` (raw SQL) | INSTANT | continue |
| ↳ with `DROP CHECK …`, which kilit cannot read yet | not answered yet | – |
| Set the table comment<br>`$table->comment('Orders placed by users')` | INPLACE | continue |

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

### MyISAM tables

When the schema or the migration says `ENGINE=MyISAM` for a table, kilit
answers from separate measurements. They ran on MySQL 8.0.46 and 8.4.13, with
the same result on both. Laravel writes `engine = MyISAM` only in
`Schema::create`; `mysqldump` writes `ENGINE=MyISAM` for every table.

| Change | MySQL does | Writes |
| --- | --- | --- |
| Add a column, nullable or `NOT NULL` with a default<br>`$table->string('coupon')->nullable()` | COPY, copies every row | **wait** |
| Drop a column<br>`$table->dropColumn('note')` | COPY, copies every row | **wait** |
| ↳ when a `FOREIGN KEY` clause names it⁶ | COPY, copies every row | **wait** |
| Rename a column<br>`$table->renameColumn('note', 'memo')` | INSTANT | continue |
| Set a column default<br>`ALTER COLUMN … SET DEFAULT` (raw SQL) | INSTANT | continue |
| Change an `INT` to a `BIGINT`<br>`$table->bigInteger('amount')->change()` | COPY, copies every row | **wait** |
| Add an index<br>`$table->index('amount')` | COPY, copies every row | **wait** |
| Add a unique index<br>`$table->unique('amount')` | COPY, copies every row | **wait** |
| ↳ when values repeat | fails, error 1062 | – |
| Add an index or a unique index on a key longer than 1000 bytes⁷<br>`$table->string('code', 251)->index()` | fails, error 1071 | – |
| Add a FULLTEXT index<br>`$table->fullText('note')` | COPY, copies every row | **wait** |
| Drop an index<br>`$table->dropIndex('orders_amount_index')` | COPY, copies every row | **wait** |
| Add a foreign key⁶<br>`$table->foreign('user_id')->references('id')->on('users')` | COPY, copies every row | **wait** |
| ↳ when an `INT` column points at a `BIGINT UNSIGNED` one, or the parent column has no index | COPY, copies every row | **wait** |
| Drop a foreign key<br>`$table->dropForeign(['user_id'])` | COPY, copies every row | **wait** |
| Anything else, or when kilit cannot tell what holds | not answered yet | – |

⁶ MySQL reads a `FOREIGN KEY` clause on a MyISAM table and ignores it: no key
is kept, no error comes, so a type mismatch or a missing parent index does not
fail as it does on InnoDB. MySQL adds a plain index named after the constraint,
or after the column when the key has no name, unless an index of exactly that
column exists; a column-level `REFERENCES` adds none.\
⁷ A MyISAM key may be 1000 bytes long. In `utf8mb4`, `VARCHAR(250)` is exactly
1000 bytes and is accepted; `VARCHAR(251)` is 1004 and fails. kilit counts
bytes only for integer columns and `utf8mb4` `VARCHAR` columns, assumes strict
SQL mode (the MySQL default), and gives no answer once the input sets
`sql_mode`.

Where MySQL copies the table, `ALGORITHM=INSTANT`, `ALGORITHM=INPLACE` and
`LOCK=NONE` fail with error 1845; `ALGORITHM=COPY` runs. While the copy runs,
reads go on and writes wait.

kilit gives no answer, with the reason, for every other change to a MyISAM
table: a type change other than `INT` to `BIGINT`, an index of several columns,
with a prefix or `DESC`, a key on other types or character sets, an expression
default, a generated column, and a foreign key whose index name the statement
chooses. A foreign key from an InnoDB table to a MyISAM table gets no answer
either.

Any other engine, such as MEMORY or ARCHIVE, gets no answer for any change to
its table, and neither does a table whose engine kilit cannot tell, such as one
created after the input sets `default_storage_engine` or
`default_tmp_storage_engine`. `ALTER TABLE ... ENGINE=...` is not read yet:
kilit stops trusting that table, and keeps answering for it only if it was
InnoDB and the statement names InnoDB.

### Row formats and `old_alter_table`

The rows above are for the default row format, DYNAMIC. When a table says
`ROW_FORMAT=COMPRESSED`, `COMPACT` or `REDUNDANT`, or has a `KEY_BLOCK_SIZE`,
kilit answers from separate measurements on MySQL 8.0.46 and 8.4.13, with the
same result on both. The same goes after the input runs `SET old_alter_table =
ON`. `KEY_BLOCK_SIZE=8` without a `ROW_FORMAT` makes the table COMPRESSED, as
`ROW_FORMAT=COMPRESSED` does.

| Change | MySQL does | Writes |
| --- | --- | --- |
| Add or drop a column, on a COMPRESSED table<br>`$table->dropColumn('note')` | INPLACE, rebuilds the table | continue |
| Rename a column, set a default, change an `INT` to a `BIGINT`, add, add a unique or drop an index, on a COMPRESSED table | as on a DYNAMIC table | as there |
| Every change of the first two rows, on a COMPACT or REDUNDANT table | as on a DYNAMIC table, `INSTANT` column changes included | as there |
| ↳ add an index or a unique index on a `utf8mb4` `VARCHAR(192)` or `VARCHAR(255)` column (768 bytes or more) | fails, error 1071 | – |
| Every change of the first two rows after `SET old_alter_table = ON`<br>`ALTER TABLE orders ADD COLUMN …` | COPY, copies every row | **wait** |
| ↳ with `LOCK=NONE` | fails, error 1846 | – |
| ↳ with an `ALGORITHM` other than `DEFAULT` | as with `old_alter_table` off | as there |
| Anything else, or a row format and `old_alter_table` together | not answered yet | – |

An index key on a COMPACT or REDUNDANT table may be 767 bytes: in `utf8mb4`,
`VARCHAR(191)` is 764 bytes and is accepted, `VARCHAR(192)` is 768 and fails.
kilit counts bytes for integer columns and `utf8mb4` `VARCHAR` columns only.

`KEY_BLOCK_SIZE` is measured at 8 only; 1, 2, 4 and 16 get no answer. A
`KEY_BLOCK_SIZE` next to `ROW_FORMAT=DYNAMIC`, `COMPACT` or `REDUNDANT` is
refused by MySQL (error 1031 in `CREATE TABLE`, 1478 in `ALTER TABLE`), and
kilit gives no answer for such a table. The case of 64 instant column changes,
and tables with duplicate values, were measured on DYNAMIC tables only; on
other row formats the answer carries a note saying so.

### What kilit assumes about the server

The lab measured with the defaults of MySQL 8.0.46 and 8.4.13:

- `old_alter_table` OFF
- `innodb_default_row_format` DYNAMIC
- `innodb_strict_mode` ON
- `binlog_format` ROW, with the binary log on
- strict `sql_mode`
- `foreign_key_checks` ON
- `default_storage_engine` InnoDB
- on 8.4, `restrict_fk_on_non_standard_key` ON

When the input sets one of these, kilit follows it where it was measured and
answers unknown otherwise. It cannot see a server that is configured
otherwise, and it prints no note about it for each statement.

kilit never picks the server from a schema dump. It does read every
`-- Server version` line of the `--schema` file, such as the one a plain
`mysqldump` writes (a joined dump can have several; each version counts once),
and never the `Distrib` line, which is the version of the `mysqldump` client:

- A MySQL series other than the one being checked: a warning, such as `the
  schema comes from MySQL 8.0.36; answers are for mysql-8.4.13`. In JSON it is
  a `server-mismatch` entry of `warnings`.
- A version text kilit cannot read: a warning that names it. In JSON it is a
  `server-mismatch` entry too.
- MariaDB: kilit has no MariaDB measurements, so it stops with exit status 2
  unless you give `--server`. With `--server` it warns and checks against that
  MySQL server.

`php artisan schema:dump` runs `mysqldump` without comments, so a Laravel dump
has no such line and kilit cannot warn.

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
kilit check --schema schema.sql migration.sql   # several files run in order, each in a session of its own
kilit check --server mysql-8.0 migration.sql    # a series: the newest release measured in it
kilit check --server mysql-8.0.46 migration.sql # or one release
kilit servers                                   # the servers kilit has results for
```

`--server` takes a release (`mysql-8.4.13`) or a series (`mysql-8.4`,
`mysql-8.0`), which means the newest release the lab measured in that series.
Without it kilit uses the series `mysql-8.4`, the 8.4 LTS. The default does not
move when the lab measures another series; `kilit servers` marks it. The header
of every report names the exact release.

On Windows, pipe in WSL, Git Bash or `cmd`, which pass Laravel's output
through as it is. PowerShell before 7.4 re-encodes text in a pipe and with `>`:
kilit still reads the statement mark that this garbles, but text in other
languages inside your SQL can come through garbled.

### When Laravel stops, and what `--pretend` does not run

A line without dots counts as a migration name only in Laravel's file-name
form, `2026_11_01_000002_add_note_to_orders_table`. The statement mark is `⇂`,
or `?`, `???` and the byte forms PowerShell leaves in single-byte code pages.

If a migration throws, Laravel prints its name, none of its statements and an
error screen, and stops. kilit recognizes the Collision screen and the Symfony
one. Nothing on it is read as a migration or a statement. kilit says one of:

- `Laravel stopped with an error in <migration>; its statements and the
  migrations after it were not seen`
- `Laravel stopped with an error after <migration>; the migrations after it
  were not seen`
- `Laravel stopped with an error in <migration> before it printed any
  migration; no statement was seen`
- `Laravel stopped with an error before it printed any migration; no
  statement was seen`

The migration it stopped in is named and not listed. Statements printed before
the error keep their answers. A syntax error in a migration's PHP stops Laravel
before it prints anything else, so the error screen is all there is. The exit
status is 2, also without `--strict`. With several input files, the line starts
with the file's name, as in `migrations/b.txt: Laravel stopped with an error in
<migration>; ...`. In JSON the message is an entry of `warnings` with the id
`laravel-error`, the input's name in `file` and no tables, before the
`metadata-lock` warning.

Under `--pretend` every read returns nothing. A migration such as
`if (Schema::hasTable('orders')) { ... }` prints the `select` and none of the
statements it guards, or prints them where they would not run. kilit adds a
note to the first read of each migration: `select`, `show`, `describe`, `desc`,
`explain`, `with`, `table` or `values`. Answers and the exit status do not
change. In JSON the note is in that statement's `notes`.

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
| 0 | nothing that --fail-on counts was found |
| 1 | a statement fails or makes writes wait, and --fail-on counts it |
| 2 | kilit could not check everything: a bad option, a file it cannot read, an unknown server, a MariaDB schema without --server, lines of the input it does not understand, or Laravel stopping with an error |
| 3 | the only statements --fail-on counts are unknown: kilit cannot tell what they do |

When several apply, 2 wins over 1, and 1 over 3.

`--fail-on` says what counts. It takes a comma-separated list of `fails`,
`waits` and `unknown`; the default is `fails,waits`. `--strict` is short for
`--fail-on fails,waits,unknown`, and cannot be combined with `--fail-on`. An
empty list or any other word is a usage error, exit status 2.

An unknown statement is usually a change the lab has not measured, not a
mistake in the migration, so by default it does not stop a build. When a run
passes with unknown statements that are not counted, the text ends with one
line saying so:

```text
1 statement: 1 unknown
1 unknown statement is not counted in the exit status; --fail-on fails,waits,unknown counts it
```

Three ways to use it in a pipeline:

```sh
kilit check --schema schema.sql migrations.sql                  # stop on failures and waits
kilit check --strict --schema schema.sql migrations.sql         # also stop on unknown
kilit check --fail-on fails --schema schema.sql migrations.sql  # accept waits, for small tables
```

The exit status follows the server you check against. When another measured
server answers a statement differently, a note says so, for example `on
mysql-8.0.46: fails with error 1822`, and the JSON has it in `other_servers`.
That note never changes the exit status.

### JSON output

`--format json` prints the same answers as one JSON document, with the keys in
this order. The text output is for people and may change in any release; the
JSON is the one to parse.

`format_version` is `"1.0"`. A minor increase (`"1.1"`) adds fields. A major
increase (`"2.0"`) removes, renames or changes one. Ignore fields you do not
know, and reject a major version you do not support.

| Key | Holds |
| --- | --- |
| `format_version` | the version of this format |
| `kilit` | kilit's version |
| `server` | the exact release checked against, such as `mysql-8.4.13` |
| `schema` | `null` without `--schema`; else `file`, `tables` (names) and `lost` (`table` and `reason` of each table kilit stopped trusting) |
| `sources` | one entry per migration, or per SQL file: `name`, `kind` (`laravel_migration` or `sql`) and `statements` |
| `unreadable` | lines of the input kilit does not understand: `input`, `line`, `text` |
| `warnings` | `id`, `tables`, `message`, and `file` on a `laravel-error` |
| `summary` | counts: `statements`, `fails`, `writes_wait`, `writes_continue`, `new_table`, `unknown`, `not_checked` |
| `fail_on` | what counted for the exit status, from `fails`, `waits`, `unknown` |
| `exit_code` | the exit status |

Each statement has `line`, `sql` and `impact` (`fails`, `writes_wait`,
`writes_continue`, `new_table`, `unknown` or `not_checked`). These keys come
only when there is something to say, and are left out otherwise:

| Key | Holds |
| --- | --- |
| `case` | the kind of change the lab measured |
| `algorithm` | `INSTANT`, `INPLACE` or `COPY` |
| `rebuilds_table` | `true` or `false` |
| `copies_rows` | `true` or `false` |
| `writes` | `allowed`, `blocked` or `unknown` |
| `error` | the MySQL error number, when the statement fails |
| `evidence` | the lab scenarios behind the answer |
| `notes` | what kilit worked out, and situations that change the answer |
| `reason` | why the statement is unknown or not checked |
| `other_servers` | where another measured server answers differently: `server`, `impact` and `answer`, the line the text report would print for that server |

A warning has one of three ids: `server-mismatch` (the schema dump names
another MySQL series, or MariaDB with `--server`, or a version kilit cannot
read), `laravel-error` (Laravel
stopped with an error; `file` names the input) and `metadata-lock` (`tables`
names the tables).

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
A scenario can make `orders` a MyISAM table. MyISAM has no table id and no row
versions, so the lab reads a copy from the server's count of `copy to tmp table`
stages.
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

**The schema** comes from `CREATE TABLE` statements, such as a schema dump,
with each table's engine. kilit follows every statement of the migration
through it: columns, indexes and foreign keys are added, changed, renamed and
dropped, so each statement is judged against the tables as they are at that
point. When a statement changes a table in a way kilit cannot follow, such as
one it cannot read, kilit stops trusting what it knew about that table instead
of guessing.

**The checker** places each change in a *case*: a kind of change, together with
the lab scenarios that measured it. For a `MODIFY` or `CHANGE`, kilit compares
the new definition with the column's current one, character set and collation
included, and the one thing that changes decides the case. The scenarios of a
case must agree on each server, or kilit refuses to answer. The two servers
may give different answers; kilit answers for the server you pick with
`--server`, and adds a note where the other one differs. A case can also
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
- Only MySQL 8.0.46 and 8.4.13 are measured. MariaDB is not supported: a
  MariaDB schema without `--server` stops kilit with exit status 2.
- kilit has measured InnoDB and, for the changes listed above, MyISAM. A table
  with no `ENGINE` is taken as InnoDB, the server default, and kilit does not
  read `default_storage_engine` from a `--schema` file, so a table created
  under a different default there is still taken as InnoDB.
- sqlparser 0.63 cannot read `CONVERT TO CHARACTER SET`, `RENAME INDEX`,
  `ALGORITHM=NOCOPY` or `OPTIMIZE TABLE` yet, so kilit reports them as
  unreadable. It reads an `ALTER TABLE` made only of `COMMENT`, `ENGINE`,
  `ROW_FORMAT` and `KEY_BLOCK_SIZE` itself. Only the comment is measured:
  changing the engine, the row format or the key block size gets no answer,
  though the table stays known. It becomes unknown when MySQL may refuse the
  change because of the table's keys or foreign keys.
- Several input files are several sessions, as `mysql < a.sql` and
  `mysql < b.sql` are. `foreign_key_checks`, `sql_log_bin` and
  `old_alter_table` start from the MySQL defaults in each file, and a note
  names the earlier file's `SET`. After a `SET GLOBAL` or `SET PERSIST` in
  an earlier file, the setting is unknown in the later ones. The tables carry
  over, except temporary tables. A `--pretend` file is one session.
- "Writes continue" describes the change itself. Every `ALTER TABLE` also
  waits for the metadata lock first; kilit warns about it, but cannot know
  which transactions will be open when you deploy. The warning covers `ALTER
  TABLE`, `CREATE INDEX` and `DROP INDEX ... ON` only. `DROP TABLE`, `RENAME
  TABLE`, `TRUNCATE TABLE` and `LOCK TABLES` do not get it yet.
- On Windows, the marks that PowerShell leaves in the double-byte code pages
  (932, 936, 949, 950) and in 869 and 874 are not known.
- `--pretend` does not print every read: `DB::getPdo()->query()`, Laravel 10's
  Doctrine introspection and the affected-row counts of writes are not seen.
  A `with ... update` gets the read note too. An error raised outside any
  migration, after a migration that printed no statements, is blamed on that
  migration.
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
- [ ] Prebuilt binaries for Linux, macOS and Windows on GitHub Releases, and a
  GitHub Action. No crate is published to crates.io

## Project layout

```text
crates/kilit/        the kilit command
crates/kilit-core/   reads migrations and checks them; no I/O
crates/kilit-lab/    measures schema changes on real MySQL servers
scenarios/           one TOML file per measured change, and the lab's tables
matrix/              the lab's results, one JSON file per server version
compose.yaml         the MySQL servers the lab runs against
```

kilit-core is kilit's engine, not a library: its API may change in any release.
No crate of this workspace is published to crates.io.

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
