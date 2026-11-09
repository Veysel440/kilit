# Measured behavior

Every row on this page is one kind of change that kilit-lab ran on real MySQL
servers: 8.0.46 and 8.4.13. The lab ran each of the 240 scenarios in
[`scenarios/`](../scenarios/) on both servers, in six variants, and kept the
result in [`matrix/`](../matrix/). The scenarios are grouped into 77 case ids:
31 for InnoDB, 12 for MyISAM, 26 for row formats and 8 for `old_alter_table`.

The tables are InnoDB tables of 1,000 rows (`orders`) and 100 rows (`users`).
A table whose `CREATE TABLE` names no `ENGINE` is taken as InnoDB, the server
default. The two servers mostly behave the same. Where they differ, the row
says which one does what.

- [How to read the tables](#how-to-read-the-tables)
- [Server settings the lab measured with](#server-settings-the-lab-measured-with)
- [Columns, indexes and foreign keys](#columns-indexes-and-foreign-keys)
- [Changing a column with `->change()`](#changing-a-column-with--change)
- [MyISAM tables](#myisam-tables)
- [Row formats and `old_alter_table`](#row-formats-and-old_alter_table)
- [Tables created without `ROW_FORMAT` under COMPACT](#tables-created-without-row_format-under-compact)
- [Where the rows come from](#where-the-rows-come-from)

## How to read the tables

The first cell names the change, with a Laravel example under it where one
exists. A row that starts with `↳` is the same change in a narrower situation.
The `Case` column is the case id in `crates/kilit-core/src/check.rs` that holds
the scenarios behind the row. For a row that starts with `↳`, the case is
followed by the exception, the situation in which the case was measured
again. A row with the case `none` is a shape the lab has not measured: kilit
answers `UNKNOWN` and prints the reason.

What the words mean:

- **INSTANT** changes only the table's metadata.
- **INPLACE** works on the existing table. Where it rebuilds the table, it
  rewrites every row, which takes time on a large table even while writes
  continue.
- **COPY** builds a new table and copies every row into it.
- Writes **wait** when the server refuses `LOCK=NONE` for the change: inserts,
  updates and deletes on the table wait until it finishes. Reads go on.
- `fails, error N` is the error number MySQL returns. The statement changes
  nothing.

Some situations cannot be seen in a schema: the rows a table holds, or the 64
instant column changes a table has had since it was last rebuilt. kilit prints
them as notes under the answer, not as the answer. The rows for them are here
because the lab measured them.

The text report ends the line `measured as <case> (N lab scenarios)`. N counts
the scenarios behind the answer: the case's, or the exception's when an
exception decides the answer.

## Server settings the lab measured with

The lab measured with the defaults of MySQL 8.0.46 and 8.4.13:

- `old_alter_table` OFF
- `innodb_default_row_format` DYNAMIC
- `innodb_strict_mode` ON
- `binlog_format` ROW, with the binary log on
- strict `sql_mode`
- `foreign_key_checks` ON
- `default_storage_engine` InnoDB
- on 8.4, `restrict_fk_on_non_standard_key` ON

What kilit does when the input names a setting:

- `foreign_key_checks` and `old_alter_table` are followed: the answers after a
  `SET` are the ones measured with that value. `sql_log_bin = 1` keeps the
  answer; `0`, `OFF`, `DEFAULT`, `GLOBAL` or a form kilit cannot read makes the
  answers that depend on it `UNKNOWN`. Set globally or to `DEFAULT`,
  `old_alter_table` makes every later `ALTER TABLE` `UNKNOWN`.
- `binlog_format`, `innodb_default_row_format`, `innodb_strict_mode`,
  `default_storage_engine` and `restrict_fk_on_non_standard_key`, once named,
  make `UNKNOWN` the answers they could change: expression defaults, the row
  format or engine of a new table, a foreign key's parent index.
- `sql_mode`: a key that may be too long on a MyISAM table is `UNKNOWN`. The
  notes that assume strict mode say so and stay.

kilit cannot see a server that is configured otherwise, and it prints no note
about it for each statement.

## Columns, indexes and foreign keys

| Change | MySQL does | Writes | Case |
| --- | --- | --- | --- |
| Add a column¹<br>`$table->string('coupon')->nullable()` | INSTANT | continue | `add-column` |
| ↳ when the table has a FULLTEXT index | COPY, copies every row | **wait** | `add-column`, exception: FULLTEXT index |
| ↳ after 64 instant column changes | INPLACE, rebuilds the table | continue | `add-column`, exception: 64 changes |
| ↳ with `DEFAULT (UUID())`, `(RAND())`, `(UUID_SHORT())`, `(SYSDATE())` or `(USER())` | fails, error 1674 (binary log on) | – | `add-column-binlog-unsafe-default` |
| ↳ with `DEFAULT (NOW())`, `(CURRENT_TIMESTAMP)`, `(CURDATE())`, `(CURRENT_DATE + INTERVAL 1 YEAR)` or `(JSON_ARRAY())`<br>`$table->date('due_on')->useCurrent()` | COPY, copies every row | **wait** | `add-column-expression-default` |
| Drop a column<br>`$table->dropColumn('note')` | INSTANT | continue | `drop-column` |
| ↳ when the table has a FULLTEXT index | COPY, copies every row | **wait** | `drop-column`, exception: FULLTEXT index |
| ↳ when the column is in an index, alone or with others, or has a UNIQUE index of its own, and the table has no FULLTEXT index<br>`$table->dropColumn('status')` (in `orders_status_index` in the example schema) | INPLACE, rebuilds the table | continue | `drop-column`, exception: indexed column |
| ↳ when the column is the only column of the only FULLTEXT index, which goes with it | INPLACE, rebuilds the table | continue | `drop-column`, exception: indexed column |
| ↳ when the column is in a foreign key<br>`$table->dropColumn('user_id')` before `$table->dropForeign(['user_id'])` | fails, error 1828 | – | `drop-column`, exception: foreign key column |
| ↳ when the column is in a foreign key of several columns or with the checks off, the primary key, a functional index or a UNIQUE index with other columns | UNKNOWN | – | none |
| ↳ when a generated column uses it | fails, error 3108 | – | `drop-column`, exception: generated column source |
| ↳ when a CHECK constraint uses it and another column | fails, error 3959 | – | `drop-column`, exception: CHECK with another column |
| Rename a column<br>`$table->renameColumn('note', 'memo')` | INSTANT | continue | `rename-column` |
| ↳ when a generated column uses it | fails, error 3108 | – | `rename-column`, exception: generated column source |
| ↳ when a CHECK constraint uses it, with other columns or alone | fails, error 3959 | – | `rename-column`, exception: CHECK uses the column |
| Set a column default<br>`ALTER COLUMN … SET DEFAULT` (raw SQL) | INSTANT | continue | `set-column-default` |
| Add an index<br>`$table->index('amount')` | INPLACE | continue | `add-index` |
| ↳ on `LOWER()` of a VARCHAR column (functional index)<br>`$table->rawIndex('(lower(email))', 'users_lower_email_index')` | INPLACE | **wait** | `add-functional-index` |
| ↳ on a `TEXT`, `TINYTEXT`, `MEDIUMTEXT`, `LONGTEXT` or `BLOB` column without a prefix length<br>`$table->text('body')->index()` | fails, error 1170 | – | `add-index`, exception: BLOB or TEXT without prefix |
| Add a unique index<br>`$table->unique('legacy_code')` | INPLACE | continue | `add-unique-index` |
| ↳ when values repeat | fails, error 1062 | – | `add-unique-index`, exception: duplicate values |
| ↳ on a `TEXT`, `TINYTEXT`, `MEDIUMTEXT`, `LONGTEXT` or `BLOB` column without a prefix length<br>`$table->text('body')->unique()` | fails, error 1170 | – | `add-unique-index`, exception: BLOB or TEXT without prefix |
| Add a FULLTEXT index<br>`$table->fullText('note')` | INPLACE, rebuilds the table | **wait** | `add-fulltext-index` |
| ↳ when the table already has one | INPLACE | **wait** | `add-fulltext-index`, exception: FULLTEXT index |
| Drop an index<br>`$table->dropIndex('orders_amount_index')` | INPLACE | continue | `drop-index` |
| ↳ when a foreign key needs it<br>`$table->dropIndex('orders_user_id_foreign')` before `$table->dropForeign(['user_id'])` | fails, error 1553 | – | `drop-index`, exception: only index of a foreign key |
| Add a foreign key<br>`$table->foreign('user_id')->references('id')->on('users')` | COPY, copies every row | **wait** | `add-foreign-key` |
| ↳ when a row points to a missing parent | fails, error 1452 | – | `add-foreign-key`, exception: orphan row |
| ↳ when the parent column is BIGINT UNSIGNED and the column is an INT, INT UNSIGNED, BIGINT or smaller unsigned integer, or the parent is INT UNSIGNED and the column BIGINT UNSIGNED | fails, error 3780 | – | `add-foreign-key`, exception: integer mismatch, narrower parent |
| ↳ when the parent column has a primary key or UNIQUE index of its own, is utf8mb4 with its default collation (`utf8mb4_0900_ai_ci`), and the column is a VARCHAR of the same length in latin1 or `utf8mb4_bin` | fails, error 3780 | – | `add-foreign-key`, exception: text mismatch |
| ↳ when no index of the parent table starts with the parent column, the column is only second in an index, or the index covers only a shorter prefix such as `name(10)` | fails, error 1822 on 8.0.46, 6125 on 8.4.13 | – | `add-foreign-key`, exception: no parent index |
| ↳ when the only index that starts with the parent column is a plain one, a plain DESC one, or a longer one | COPY, copies every row on 8.0.46; fails, error 6125 on 8.4.13 | **wait** on 8.0.46, – on 8.4.13 | `add-foreign-key`, exception: non-unique parent index |
| ↳ when the two columns differ in a way not measured, the key has several columns without a matching UNIQUE index or primary key, or the parent index has a prefix as long as the column or longer, is FULLTEXT only, or part of a longer primary key, or the parent column has a foreign key of its own | UNKNOWN | – | none |
| ↳ with the checks off²<br>`Schema::withoutForeignKeyConstraints(fn () => …)`, or `Schema::disableForeignKeyConstraints()` and `Schema::enableForeignKeyConstraints()` around the call | INPLACE | continue | `add-foreign-key-checks-off` |
| ↳ with the checks off, when the two columns visibly differ, the parent has no fitting index, the key points at its own table or the shape is not measured | UNKNOWN | – | none |
| Drop a foreign key<br>`$table->dropForeign(['user_id'])` | INPLACE | continue | `drop-foreign-key` |
| Add a CHECK constraint<br>`ADD CONSTRAINT … CHECK (…)` (raw SQL) | COPY, copies every row | **wait** | `add-check-constraint` |
| ↳ when a row breaks it | fails, error 3819 | – | `add-check-constraint`, exception: row breaks it |
| ↳ when its expression has a string, a function call, `IN`, `BETWEEN` or `LIKE` or no column, or kilit sees a missing, AUTO_INCREMENT, generated or foreign key column, or a CHECK name the database has | UNKNOWN | – | none |
| ↳ with `NOT ENFORCED`<br>`ADD CONSTRAINT … CHECK (…) NOT ENFORCED` (raw SQL), under the same conditions | INSTANT | continue | `add-not-enforced-check-constraint` |
| Drop a CHECK constraint<br>`DROP CONSTRAINT …` or `DROP CHECK …` (raw SQL) | INSTANT | continue | `drop-check-constraint` |
| ↳ when the name belongs to a foreign key or an index, does not exist, or is shared by a CHECK and another constraint | UNKNOWN | – | none |
| Make a CHECK constraint not enforced<br>`ALTER CHECK … NOT ENFORCED` or `ALTER CONSTRAINT … NOT ENFORCED` (raw SQL) | INSTANT | continue | `alter-check-constraint` |
| ↳ with `ENFORCED` on a CHECK that is already enforced | INSTANT | continue | `alter-check-constraint` |
| Enforce a CHECK constraint again<br>`ALTER CHECK … ENFORCED` or `ALTER CONSTRAINT … ENFORCED` (raw SQL) | COPY, copies every row | **wait** | `enforce-check-constraint` |
| ↳ when a row breaks it | fails, error 3819 | – | `enforce-check-constraint`, exception: row breaks it |
| Set the table comment<br>`$table->comment('Orders placed by users')` | INPLACE | continue | `set-table-comment` |

¹ Seven ways of adding a column were measured, and all are INSTANT: nullable,
`NOT NULL` with and without a default, `AFTER`, `FIRST`, with a character set,
collation and comment, and a `TIMESTAMP` defaulting to `CURRENT_TIMESTAMP`.\
² Even when a row points to a missing parent: the foreign key is added, and the
row stays as it is, unchecked. Laravel prints `SET FOREIGN_KEY_CHECKS=0;`
before the call and `SET FOREIGN_KEY_CHECKS=1;` after it.

## Changing a column with `->change()`

Laravel turns `->change()` into `MODIFY`, which replaces the column's whole
definition. kilit compares it with the column's current definition, from your
schema, and answers for what actually changes. Without a schema it lists what
the lab measured instead of answering.

| Change | MySQL does | Writes | Case |
| --- | --- | --- | --- |
| Change between integer types³<br>`$table->bigInteger('amount')->change()` | COPY, copies every row | **wait** | `change-integer-type` |
| ↳ when the column is in a foreign key | fails, error 3780 | – | `change-integer-type`, exception: foreign key column |
| ↳ when a value does not fit the new type | fails, error 1264 | – | `change-integer-type`, exception: value out of range |
| Lengthen a VARCHAR, staying on one side of 255 bytes⁴<br>`$table->string('note', 63)->nullable()->change()` | INPLACE | continue | `lengthen-varchar` |
| Lengthen a VARCHAR so it crosses 255 bytes⁴<br>`$table->string('note', 64)->nullable()->change()` | COPY, copies every row | **wait** | `lengthen-varchar-past-255-bytes` |
| Shorten a VARCHAR<br>`$table->string('note', 40)->nullable()->change()` | COPY, copies every row | **wait** | `shorten-varchar` |
| ↳ when a value is longer than the new length | fails, error 1265 | – | `shorten-varchar`, exception: value too long |
| Change the character set⁵<br>`$table->string('note', 50)->charset('latin1')->nullable()->change()` | COPY, copies every row | **wait** | `change-column-charset` |
| Change the collation<br>`$table->string('note', 50)->collation('utf8mb4_bin')->nullable()->change()` | INPLACE | continue | `change-column-collation` |
| ↳ when the column is in an index | COPY, copies every row | **wait** | `change-column-collation`, exception: indexed column |
| Make a column nullable<br>`$table->integer('amount')->nullable()->change()` | INPLACE, rebuilds the table | continue | `make-column-nullable` |
| Make a column NOT NULL<br>`$table->string('note', 50)->nullable(false)->change()` | INPLACE, rebuilds the table | continue | `make-column-not-null` |
| ↳ when a row holds NULL | fails, error 1138 | – | `make-column-not-null`, exception: NULL in a row |
| Give, change or drop a default<br>`$table->integer('amount')->default(0)->change()` | INSTANT | continue | `modify-column-default` |
| Move a column<br>`$table->string('note', 50)->nullable()->after('user_id')->change()` | INPLACE, rebuilds the table | continue | `move-column` |
| Rename a column with `CHANGE`<br>`ALTER TABLE … CHANGE …` (raw SQL) | INSTANT | continue | `rename-column` |
| Keep the same definition<br>`->change()` with nothing new | INSTANT | continue | `modify-same-definition` |

³ `INT` to `BIGINT`, to `SMALLINT` and to `INT UNSIGNED`.\
⁴ In `utf8mb4` a character takes up to 4 bytes, so `VARCHAR(63)` is 252 bytes
and `VARCHAR(64)` is 256; in `latin1`, one byte each. Past 255 bytes, MySQL
stores each value's length in 2 bytes instead of 1. A change that stays on one
side of 255 bytes is INPLACE, `VARCHAR(255)` to `VARCHAR(300)` in `utf8mb4`
included (both are past 255 bytes); one that crosses it is COPY. Both edges
were measured in both character sets.\
⁵ Including a `MODIFY` that names no character set on a column whose set
differs from the table's: the column takes the table's, and MySQL copies the
table to convert it.

A statement with several changes, including a `MODIFY` that changes several
things about a column, is `UNKNOWN`: the lab has measured single changes only.
The same goes for a change of type other than between integer types, such as
`VARCHAR` to `TEXT`.

## MyISAM tables

When the schema or the migration says `ENGINE=MyISAM` for a table, kilit
answers from separate measurements. They ran on MySQL 8.0.46 and 8.4.13, with
the same result on both. Laravel writes `engine = MyISAM` only in
`Schema::create`; `mysqldump` writes `ENGINE=MyISAM` for every table.

| Change | MySQL does | Writes | Case |
| --- | --- | --- | --- |
| Add a column, nullable or `NOT NULL` with a default<br>`$table->string('coupon')->nullable()` | COPY, copies every row | **wait** | `myisam-add-column` |
| Drop a column<br>`$table->dropColumn('note')` | COPY, copies every row | **wait** | `myisam-drop-column` |
| ↳ when a `FOREIGN KEY` clause names it⁶ | COPY, copies every row | **wait** | `myisam-drop-column` |
| Rename a column<br>`$table->renameColumn('note', 'memo')` | INSTANT | continue | `myisam-rename-column` |
| Set a column default<br>`ALTER COLUMN … SET DEFAULT` (raw SQL) | INSTANT | continue | `myisam-set-column-default` |
| Change an `INT` to a `BIGINT`<br>`$table->bigInteger('amount')->change()` | COPY, copies every row | **wait** | `myisam-change-integer-type` |
| Add an index<br>`$table->index('amount')` | COPY, copies every row | **wait** | `myisam-add-index` |
| Add a unique index<br>`$table->unique('amount')` | COPY, copies every row | **wait** | `myisam-add-unique-index` |
| ↳ when values repeat | fails, error 1062 | – | `myisam-add-unique-index`, exception: duplicate values |
| Add an index or a unique index on a key longer than 1000 bytes⁷<br>`$table->string('code', 251)->nullable()->index()` | fails, error 1071 | – | `myisam-add-key-too-long` |
| Add a FULLTEXT index<br>`$table->fullText('note')` | COPY, copies every row | **wait** | `myisam-add-fulltext-index` |
| Drop an index<br>`$table->dropIndex('orders_amount_index')` | COPY, copies every row | **wait** | `myisam-drop-index` |
| Add a foreign key⁶<br>`$table->foreign('user_id')->references('id')->on('users')` | COPY, copies every row | **wait** | `myisam-add-foreign-key` |
| ↳ when an `INT` column points at a `BIGINT UNSIGNED` one, or the parent column has no index | COPY, copies every row | **wait** | `myisam-add-foreign-key` |
| Drop a foreign key<br>`$table->dropForeign(['user_id'])` | COPY, copies every row | **wait** | `myisam-drop-foreign-key` |
| Anything else, or when kilit cannot tell what holds | UNKNOWN | – | none |

⁶ MySQL reads a `FOREIGN KEY` clause on a MyISAM table and ignores it: no key
is kept, no error comes, so a type mismatch or a missing parent index does not
fail as it does on InnoDB. MySQL adds a plain index named after the constraint,
or after the column when the key has no name, unless an index of exactly that
column exists; a column-level `REFERENCES` adds none.\
⁷ A MyISAM key may be 1000 bytes long. In `utf8mb4`, `VARCHAR(250)` is exactly
1000 bytes and is accepted; `VARCHAR(251)` is 1004 and fails. kilit counts
bytes only for integer columns and `utf8mb4` `VARCHAR` columns, assumes strict
SQL mode (the MySQL default), and gives no answer for a key that may be too
long once the input sets `sql_mode`.

Where MySQL copies the table, `ALGORITHM=INSTANT`, `ALGORITHM=INPLACE` and
`LOCK=NONE` fail with error 1845; `ALGORITHM=COPY` runs. While the copy runs,
reads go on and writes wait.

kilit answers `UNKNOWN`, with the reason, for every other change to a MyISAM
table: a type change other than `INT` to `BIGINT`, an index of several columns,
with a prefix or `DESC`, a key on other types or character sets, an expression
default, a generated column, and a foreign key whose index name the statement
chooses. A foreign key from an InnoDB table to a MyISAM table is `UNKNOWN`
too.

Any other engine, such as MEMORY or ARCHIVE, is `UNKNOWN` for every change to
its table, and so is a table whose engine kilit cannot tell, such as one
created after the input sets `default_storage_engine` or
`default_tmp_storage_engine`. `ALTER TABLE ... ENGINE=...` is read: the change
itself is `UNKNOWN`, the table stays known, and later statements are answered
for the new engine. kilit stops trusting the table only when MySQL may refuse
the change because of the table's keys or foreign keys.

## Row formats and `old_alter_table`

The rows above are for the default row format, DYNAMIC. When a table says
`ROW_FORMAT=COMPRESSED`, `COMPACT` or `REDUNDANT`, or has a `KEY_BLOCK_SIZE`,
kilit answers from separate measurements on MySQL 8.0.46 and 8.4.13, with the
same result on both. The same goes after the input runs `SET old_alter_table =
ON`. `KEY_BLOCK_SIZE=8` without a `ROW_FORMAT` makes the table COMPRESSED, as
`ROW_FORMAT=COMPRESSED` does.

| Change | MySQL does | Writes | Case |
| --- | --- | --- | --- |
| Add or drop a column, on a COMPRESSED table<br>`$table->dropColumn('note')` | INPLACE, rebuilds the table | continue | `compressed-add-column`, `compressed-drop-column` |
| Rename a column, set a default, change an `INT` to a `BIGINT`, add, add a unique or drop an index, on a COMPRESSED table | as on a DYNAMIC table | as there | `compressed-rename-column`, `compressed-set-column-default`, `compressed-change-integer-type`, `compressed-add-index`, `compressed-add-unique-index`, `compressed-drop-index` |
| Every change of the first two rows, on a COMPACT or REDUNDANT table | as on a DYNAMIC table, `INSTANT` column changes included | as there | `compact-*` and `redundant-*` cases of the same changes |
| ↳ add an index or a unique index on a `utf8mb4` `VARCHAR(192)` or `VARCHAR(255)` column (768 bytes or more) | fails, error 1071 | – | `compact-add-key-too-long`, `redundant-add-key-too-long` |
| Every change of the first two rows after `SET old_alter_table = ON`<br>`ALTER TABLE orders ADD COLUMN …` | COPY, copies every row | **wait** | `old-alter-table-*` cases of the same changes |
| ↳ with `LOCK=NONE` | fails, error 1846 | – | the forced variant of the same case |
| ↳ with an `ALGORITHM` other than `DEFAULT` | as with `old_alter_table` off | as there | the forced variant of the same case |
| Anything else, or a row format and `old_alter_table` together | UNKNOWN | – | none |

An index key on a COMPACT or REDUNDANT table may be 767 bytes: in `utf8mb4`,
`VARCHAR(191)` is 764 bytes and is accepted, `VARCHAR(192)` is 768 and fails.
kilit counts bytes for integer columns and `utf8mb4` `VARCHAR` columns only.

`KEY_BLOCK_SIZE` is measured at 8 only; 1, 2, 4 and 16 are `UNKNOWN`. A
`KEY_BLOCK_SIZE` next to `ROW_FORMAT=DYNAMIC`, `COMPACT` or `REDUNDANT` is
refused by MySQL unless `innodb_strict_mode` is OFF, and kilit answers
`UNKNOWN` for such a table. The case of 64 instant column
changes, and tables with duplicate values, were measured on DYNAMIC tables
only; on other row formats the answer carries a note saying so.

## Tables created without `ROW_FORMAT` under COMPACT

kilit answers an InnoDB table that has no `ROW_FORMAT` as a DYNAMIC table. A
table created without one while the default row format was COMPACT (before
MySQL 5.7.9) keeps COMPACT until it is rebuilt. These facts are hand tests, not
lab scenarios, so there is no case id, and kilit does not use them.

The tests ran on MySQL 8.0.46 and 8.4.13 with the same result on both
(`innodb_strict_mode` ON, 16K pages, default `sql_mode`). One table was created
without `ROW_FORMAT` while the global `innodb_default_row_format` was COMPACT,
another with `ROW_FORMAT=COMPACT` written, and a control after the default was
set back to DYNAMIC. Each table had a `utf8mb4` `VARCHAR(255)`, `VARCHAR(191)`
and `VARCHAR(192)` column and three rows.

- `SHOW CREATE TABLE` and `mysqldump --no-data` print the table without
  `ROW_FORMAT`, the same text as for the DYNAMIC control. The table with
  `ROW_FORMAT=COMPACT` written shows it.
  `information_schema.INNODB_TABLES` says Compact for both tables.
- An index or unique index on `VARCHAR(255)` (1020 bytes) or `VARCHAR(192)`
  (768 bytes), by `ADD INDEX`, `ADD UNIQUE` or `CREATE INDEX`, fails with error
  1709 (`Index column size too large. The maximum column size is 767 bytes.`)
  as written, with `ALGORITHM=INPLACE`, with `ALGORITHM=INPLACE, LOCK=NONE`,
  and with `LOCK=NONE`. `ALGORITHM=INSTANT` fails with error 1845.
  `ALGORITHM=COPY` succeeds and the table becomes DYNAMIC.
- The same on the table with `ROW_FORMAT=COMPACT` written fails with error 1071
  in all six variants, as in the COMPACT rows above, and the table stays
  COMPACT after a rebuild.
- A 764-byte key (`VARCHAR(191)`) is accepted: INPLACE, no rebuild.
- `ADD COLUMN` is INSTANT, as on a DYNAMIC table.
- Any rebuild makes the table DYNAMIC: `ADD COLUMN` with `ALGORITHM=INPLACE`,
  `ALGORITHM=COPY`, `ALTER TABLE … FORCE` and `ALTER TABLE … ENGINE=InnoDB`.
  The index is accepted afterwards.

To see the real row format:

```sql
SELECT NAME, ROW_FORMAT FROM information_schema.INNODB_TABLES WHERE NAME LIKE 'mydb/%';
```

## Where the rows come from

Every row comes from a scenario in [`scenarios/`](../scenarios/) and its
results in [`matrix/`](../matrix/), one JSON file per server release. The
golden test in `crates/kilit-core/tests/golden.rs` runs every scenario of
every case through kilit, in all six variants and on both servers, and fails
if kilit's answer differs from what the server did. How the lab measures is in
[the architecture notes](architecture.md#the-lab); how to add a row is in
[CONTRIBUTING.md](../CONTRIBUTING.md#adding-a-measurement).
