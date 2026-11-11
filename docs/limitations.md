# Limitations

What kilit does not do, and what it does instead. When it does not know, it
says `UNKNOWN` and prints the reason; it never reports an unmeasured change as
safe.

- [Input](#input)
- [What is measured](#what-is-measured)
- [Tables kilit does not know](#tables-kilit-does-not-know)
- [Columns and keys kilit does not judge](#columns-and-keys-kilit-does-not-judge)
- [Defaults and row size](#defaults-and-row-size)
- [The server kilit cannot see](#the-server-kilit-cannot-see)
- [Sessions and several files](#sessions-and-several-files)
- [The metadata lock warning](#the-metadata-lock-warning)
- [Laravel](#laravel)

## Input

- sqlparser 0.63 cannot read `CONVERT TO CHARACTER SET`,
  `ALGORITHM=NOCOPY`, `OPTIMIZE TABLE` or `DROP INDEX ... ON t ALGORITHM=...`.
  Each is `UNKNOWN: kilit cannot read it`. A table named in such a statement
  is lost from then on.
- kilit reads `DROP CHECK name`, `ALTER CHECK name [NOT] ENFORCED` and
  `ALTER CONSTRAINT name [NOT] ENFORCED` itself, alone or with `ALGORITHM` and
  `LOCK`. Several changes in one statement stay `UNKNOWN`. A name that is an
  unquoted reserved word is unreadable. `DROP CHECK` without a name, with
  `IF EXISTS` or with `CASCADE`, and `ALTER CHECK` without `ENFORCED` are
  `UNKNOWN` because MySQL 8.0.46 and 8.4.13 do not accept them. If the name is
  a foreign key, an index or missing, the answer is `UNKNOWN` too. Two clauses
  on the same CHECK in one statement, `RENAME TO` beside a CHECK clause and a
  clause without a comma before `ALGORITHM` are unreadable.
- kilit reads an `ALTER TABLE` made only of `COMMENT`, `ENGINE`, `ROW_FORMAT`
  and `KEY_BLOCK_SIZE` itself. Only the comment is measured: changing the
  engine, the row format or the key block size is `UNKNOWN`, though the table
  stays known. It becomes lost when MySQL may refuse the change because of
  the table's keys or foreign keys.
- A statement that consists only of a `/*! */` comment is not seen, so a
  `SET` inside one, such as `/*!80000 SET old_alter_table = ON */`, does not
  change the answers after it.
- `CREATE TABLE` is not checked: kilit follows the table it creates, but says
  nothing about the statement itself. The one exception is syntax MySQL
  8.0.46 and 8.4.13 reject (a name in single quotes, a key part that is not a
  column, a prefix length or an expression in its own parentheses; an
  `ON UPDATE` or `DEFAULT` that does not fit the column): that statement is
  `UNKNOWN` and creates no table.
- Some refused forms are still read as if MySQL took them, and get an answer
  that MySQL would not give: an unquoted reserved word used as a name, a
  prefix longer than its column (error 1089), a key that is too long (1071),
  a `DATETIME` or `TIME` precision above 6, `YEAR` with `CURRENT_TIMESTAMP`,
  a function word such as `position` or `extract` as a key-part column name
  written right before `(`, and `NOW ()` with a space as a default or
  `ON UPDATE`.
- Some forms are unreadable to kilit, which says it could not read the
  statement: single-quoted table options that MySQL takes, such as
  `ENGINE = 'InnoDB'`, `DROP KEY 'ix'`, `RENAME INDEX` with quotes, and the
  MariaDB forms `IF EXISTS` and `IF NOT EXISTS` on
  `DROP FOREIGN KEY`, `DROP INDEX`, `ADD COLUMN` and `ADD INDEX`. A table
  named in such a statement may be lost from then on.
- Laravel's `migrate --pretend` output turns every run of spaces into one, even
  inside quotes, so kilit sees `DEFAULT 'a  b'` as `DEFAULT 'a b'`.

## What is measured

- `MODIFY` and `CHANGE` need the table's current definition. Without it,
  the statement is `UNKNOWN`: see
  [Tables kilit does not know](#tables-kilit-does-not-know).
- A statement with several changes, including a `MODIFY` that changes several
  things about a column, is `UNKNOWN`. The lab has measured single changes
  only, and MySQL picks one algorithm for the whole statement.
- Type changes other than between integer types, such as `VARCHAR` to `TEXT`,
  are `UNKNOWN`.
- Only MySQL 8.0.46 and 8.4.13 are measured. MariaDB is not supported: a
  MariaDB schema without `--server` stops kilit with exit status 2.
- The lab has measured InnoDB and, for the changes in
  [the findings](findings.md#myisam-tables), MyISAM. Any other engine is
  `UNKNOWN`.
- Row formats other than DYNAMIC, and `old_alter_table`, have their own
  measurements for a short list of changes. Anything else, and the two
  together, is `UNKNOWN`. `KEY_BLOCK_SIZE` is measured at 8 only.
- The cases of 64 instant column changes and of duplicate values were measured
  on DYNAMIC tables only; on other row formats the answer carries a note.

## Tables kilit does not know

kilit knows a table when the `--schema` file has it or an earlier statement of
the input creates it. It does not know a table when no `--schema` was given,
when the dump lacks the name, or when it lost track of the table: it could not
follow one statement, or the table was dropped or renamed. On such a table
kilit gives no answer that the table could change.

- kilit looks at every table the name may be: a plain InnoDB table, one with a
  FULLTEXT index, an indexed column, a foreign key on either side, a `TEXT` or
  `BLOB` column, the primary key, a generated column or CHECK that uses the
  column, the row size, and a MyISAM, COMPRESSED, COMPACT or REDUNDANT table
  where the lab measured the change. Then, in plain words:
  1. A failure that comes from the statement alone stays the answer.
  2. If one of these has no measured answer, the statement is `UNKNOWN`.
  3. If all give the same word, kilit keeps it, except "writes continue":
     another engine or row format that nobody measured may apply.
  4. If none lets writes continue, the answer is "writes wait" (or "fails"),
     and the algorithm and the rebuild depend on the table.
  5. Otherwise the statement is `UNKNOWN`. No case reaches this rule today.
- The reason says why kilit does not know the table (no `--schema`, not in the
  dump, lost at a line and why, dropped or renamed at a line) and gives the
  answer the lab measured on a plain InnoDB table with the DYNAMIC row format,
  with its case. The notes list the other situations and their answers. Even
  `RENAME COLUMN` and `SET DEFAULT` are `UNKNOWN`, and so is `ADD FOREIGN KEY`
  when either table is unknown.
- Today only `ADD FULLTEXT` reaches rules 3 and 4: it makes writes wait, so a
  run without `--schema` exits 1 for it. A forced `ALGORITHM=INSTANT` or
  `LOCK=NONE` fails, and `ALGORITHM=COPY` is `UNKNOWN`. A table that an
  earlier statement dropped or renamed away is `UNKNOWN` for every statement
  (MySQL gives error 1146), and so is every `DROP INDEX`.
- A Lost table whose engine is known to be MyISAM, or whose row format is not
  DYNAMIC, or after `old_alter_table` or `innodb_default_row_format` is named,
  stays `UNKNOWN` for these statements, where rules 3 and 4 could answer. It
  is cautious.
- The only failures kept on such a table come from the statement alone: error
  1674 for a nondeterministic default and error 1101 for a quoted default on
  a `TEXT`, `BLOB`, `JSON` or `GEOMETRY` column named in the statement. Every
  other failure (1828, 1553, 3959, 1091, 3780, 1822, 6125, 1118) is a note.
- With `--schema`, a table the dump lacks is `UNKNOWN` for every statement,
  1674 and 1101 included. The reason says which statement dropped or renamed
  it, with its line, or that the dump may be out of date or the table does not
  exist and MySQL refuses the statement.
- Without `--schema`, the text report says once, at the top, how many
  statements are on tables kilit does not know and how to get a dump
  (`php artisan schema:dump`, or `mysqldump --no-data`). Tables the input
  creates stay known. `CREATE TABLE IF NOT EXISTS` of a name kilit does not
  hold loses the table, because MySQL keeps an existing one.
- The default exit status does not count `UNKNOWN`, so a run without
  `--schema` exits 0 where it used to exit 1 for a foreign key. A FULLTEXT
  index still exits 1. Use `--strict` in CI (exit 3 when only unknown
  statements are found).
- A `RENAME TABLE` of a table kilit has never seen (no `--schema`) records the
  old name as gone, as `DROP TABLE` does.
- A foreign key of a table kilit does not know may point at a table kilit
  knows. Then dropping an index that serves it, or changing or dropping an
  indexed column of the known table, is `UNKNOWN`.
- A name with a database, such as `shop.orders`, next to a table `orders` that
  kilit has, is `UNKNOWN` with or without a dump: kilit does not know which
  database the statements run in. A single backticked name that holds a dot,
  such as `a.orders`, is read as a qualified name too.
- `DROP COLUMN`, `DROP INDEX` and `DROP FOREIGN KEY` of a name the known table
  does not have are `UNKNOWN`. The reason names error 1091 and the scenarios
  `drop-column-missing`, `drop-index-missing` and `drop-foreign-key-missing`.
  `DROP CHECK` and `ALTER CHECK` of a name it does not have name error 3821
  (`drop-check-missing`, `alter-check-missing`), and `DROP CONSTRAINT` and
  `ALTER CONSTRAINT` name error 3940 (`drop-constraint-missing`,
  `alter-constraint-missing`). The table stays known, since kilit's view of it
  is right whether the name existed or not. The exception: when the table
  holds an index or a foreign key without a name (MySQL named it, such as
  `orders_ibfk_1`), kilit does not know which names it has, so the drop keeps
  its measured answer and the table is lost. kilit gives no names to the keys
  of a `CREATE TABLE`.
- Five statements are followed, and the table stays known, but the statement
  itself is `UNKNOWN` because the lab has not measured it: `ALTER TABLE ...
  AUTO_INCREMENT = n` (nothing kilit models changes), `ALTER COLUMN ... DROP
  DEFAULT`, `RENAME INDEX` and `RENAME KEY` (alone), `CREATE FULLTEXT INDEX`
  (the same index as `ALTER TABLE ... ADD FULLTEXT`, whose measured answer the
  reason gives; `CREATE [UNIQUE] INDEX` keeps the answer of `ADD INDEX`), and
  `ENGINE = 'InnoDB'` on an InnoDB table. Each effect was checked by hand on
  MySQL 8.0.46 and 8.4.13. When MySQL refuses the statement on the known table
  (a missing index or column, a name already taken, `RENAME INDEX` of
  `PRIMARY`), the table stays as it is and the reason names the error. Beside
  other clauses in one statement, `RENAME INDEX` loses the table. Because
  `AUTO_INCREMENT = n` and `DROP DEFAULT` do not put rows in a table, a later
  `ALTER` of a table the input created still drops the row notes.

## Columns and keys kilit does not judge

- A `MODIFY`, `CHANGE` or `ADD COLUMN` that makes a column longer than its
  type holds is not followed as written. A `VARCHAR` or `VARBINARY` over
  65,535 bytes is refused by MySQL in strict `sql_mode` (error 1074) and the
  table stays as it was; but kilit takes that only when the input itself set a
  strict mode earlier in the same file (`STRICT_TRANS_TABLES` or
  `STRICT_ALL_TABLES` in a list it reads). The server's own `sql_mode` may not
  be strict, and Laravel's `'strict' => false` sets a non-strict one that
  `--pretend` does not print. Without that `SET`, and after a non-strict or
  unclear `sql_mode`, MySQL turns the column into `MEDIUMTEXT` or
  `MEDIUMBLOB`, and kilit loses the table. A `CHAR` or `BINARY` over 255 is
  refused in every mode (1074; run by hand on both servers). A `CREATE TABLE`
  with such a column loses the table too.
- Some shapes on a known table are `UNKNOWN` because the lab has not measured
  them. The reason names an error only where a hand test on both servers
  recorded it: `ADD COLUMN ... NOT NULL` of a `DATE`, `DATETIME` or
  `TIMESTAMP` without a default; an index on a column the table does not have
  (1072) or under a name the table has (1061); a key that kilit counts past
  3072 bytes; a prefix longer than its column (1089 on a `VARCHAR`); dropping
  one of several `FULLTEXT` indexes, a `UNIQUE` index on several columns, or a
  `UNIQUE` index of `NOT NULL` columns on a table without a primary key; a
  `MODIFY` that makes a primary key column nullable; and a `MODIFY` or
  `CHANGE` that moves a column `AFTER` itself.
- A `SET sql_mode` inside a `/*! */` comment is not seen, so a strict mode set
  earlier stays in force for kilit after `/*!40101 SET SQL_MODE='' */`. The
  over-long column rule above then keeps a table that MySQL changed.
- `ADD FULLTEXT INDEX (col DESC)` is refused by MySQL (error 1221) and kilit
  answers it and follows it. A FULLTEXT index on a `TEXT` column with
  `CHARACTER SET binary` on a DYNAMIC table gets the lab's answer where MySQL
  gives error 1283. Both are older than this version.
- The 3072-byte count does not cover a `DATE`, `DATETIME` or `TIMESTAMP` key
  part, so a key that holds one runs when MySQL may refuse it (1071). A key
  that names a column twice (error 1060) runs too. Adding a `UNIQUE` index of
  `NOT NULL` columns to a table without a primary key makes it the clustered
  key, and kilit says no rebuild.

## Defaults and row size

- A quoted default on `TEXT`, `BLOB`, `JSON` or `GEOMETRY` is a failure (error
  1101) in strict `sql_mode`, the MySQL default and Laravel's
  `'strict' => true`. kilit cannot see that a Laravel connection has
  `'strict' => false`: it sets `sql_mode` to `NO_ENGINE_SUBSTITUTION` without a
  statement in `--pretend` output, and then `TEXT` and `BLOB` with `''` are
  accepted. Such an application gets "fails" for a statement MySQL accepts. The
  note under the answer names the case. Some real Laravel applications run with
  `'strict' => false`.
- `sql_mode` is read only from a session `SET` of a quoted list. `GLOBAL`,
  `PERSIST`, an expression or a list kilit has no measurement for make the
  answer `UNKNOWN`. Each input file is a new session.
- `ALTER COLUMN ... SET DEFAULT` with a quoted literal on a `JSON` column
  fails in strict mode (error 1101) and is also refused in a non-strict mode;
  kilit answers `UNKNOWN` there. A `MODIFY` to `TEXT` with a quoted default
  after a non-strict `sql_mode` is `UNKNOWN`. Both were run by hand on MySQL
  8.0.46 and 8.4.13, but are not among kilit's scenarios.
- MySQL renames foreign key names `<table>_ibfk_<suffix>` and CHECK names
  `<table>_chk_<suffix>`, whatever the suffix, with their table (by hand on
  both servers), and kilit follows that. kilit renames them even when MySQL
  refuses the rename because a new name is too long (1059) or taken (1826,
  3822).
- The row size counts 16K pages only. It leaves out bytes MySQL keeps
  hidden: columns dropped instantly, row versions, and the `FTS_DOC_ID` a
  dropped `FULLTEXT` index leaves behind. Columns it
  cannot count (generated, long `ENUM` and `SET`, `BIT` over 16, other
  spatial types, unknown types or character sets) make a note.
- Under `--pretend` every read returns nothing, so a migration that tests the
  database (`Schema::hasColumn()`) prints statements that would not run, and
  the row kilit counts may hold columns the real run never adds. On one real
  application this gave error 1118 where the real run passes.

## The server kilit cannot see

- kilit assumes the defaults listed in
  [the findings](findings.md#server-settings-the-lab-measured-with). It
  follows `foreign_key_checks` and `old_alter_table` when the input sets them.
  Other settings the input names make `UNKNOWN` the answers they could change;
  the findings say which. A server configured otherwise is invisible to it.
- A table with no `ENGINE` is taken as InnoDB, the server default. kilit does
  not read `default_storage_engine` from a `--schema` file, so a table created
  under a different default there is still taken as InnoDB.
- kilit cannot see a table's real row format when `ROW_FORMAT` is not written.
  Such a table created before MySQL 5.7.9 may be COMPACT, where an index over
  767 bytes fails with error 1709 (as written and with `ALGORITHM=INPLACE`)
  while kilit answers as for DYNAMIC.
  `SELECT NAME, ROW_FORMAT FROM information_schema.INNODB_TABLES WHERE NAME LIKE
  'mydb/%';` shows it. The hand tests are in [the
  findings](findings.md#tables-created-without-row_format-under-compact).
- kilit does not know the server's `lower_case_table_names`. Table names are
  kept and compared as written. A name that differs from a known table only in
  letter case, in any statement that names a table (including a foreign key's
  parent, `CREATE TABLE`, `RENAME TABLE` and `DROP TABLE IF EXISTS`), is
  `UNKNOWN`: with 0, the default on Linux, it is another table; with 1 or 2,
  the defaults on Windows and macOS, it is the same one. The reason says so and
  names the setting kilit follows afterwards.
- What kilit follows after such a statement is the world in which the
  migration goes on. Where the hand tests show that the statement fails with 0
  (`ALTER TABLE`, `CREATE INDEX`, `DROP INDEX`, a single `DROP TABLE`, a single
  `RENAME TABLE` to a new name, `TRUNCATE TABLE`, a database name in another
  case, a foreign key's parent with the checks on), the migration stops there
  with 0, so kilit goes on as with 1 or 2 and applies the statement to the
  known table. Where it fails with 1 (`CREATE TABLE ORDERS` while `orders`
  exists, `RENAME TABLE orders TO ORDERS`), kilit goes on as with 0, and
  `ORDERS` is a table of its own. Where the lab's answer with 1 or 2 is a
  failure too, the tables stay as they are. A foreign key whose parent is in
  another case, with the checks off or not known, is added to the same child
  under both settings, so kilit keeps the child with the key as written.
- kilit loses the tables the statement may change, and later statements get
  the answer for a table it cannot see, with notes, when both settings go on
  and leave different tables: `DROP TABLE IF EXISTS`, `ALTER ... RENAME TO`,
  a `DROP TABLE` or `RENAME TABLE` of several tables, a rename to a name some
  table already has, `CREATE TABLE IF NOT EXISTS`, `CREATE TEMPORARY TABLE`,
  `CREATE TABLE` with foreign keys or `LIKE`, a self-reference, two names in
  one statement that differ only in case, letters the hand tests did not
  change, an unreadable statement, a name already lost this way, a statement
  after one kilit could not read (it may have created a table unseen), and a
  table of another database with the written name.
- Only ASCII letters, `Ç`/`ç` and `İ`/`I`/`i` were tested. Other letters (`ı`,
  `ß`, `Ö`, ...) are compared as before but their statements lose the table.
- With the checks off, a foreign key to `USERS` is kept as written under both
  settings. Changing the type of its column fails with 1832 with 0 (the parent
  is missing) and 3780 with 1; kilit says 3780. A dangling key such as
  `REFERENCES nosuch` with the checks off has the same gap, and is older.
- Column, index, foreign key and `CHECK` names are compared without case, as
  MySQL does on every setting.
- `shop.orders` and `orders` are different names to kilit, as before.
- `CREATE TEMPORARY TABLE` with a name that differs only in letter case is
  `UNKNOWN` and hides the table of the other spelling until the input file
  ends. `DROP TEMPORARY TABLE IF EXISTS` in another case loses that table for
  good.
- A `RENAME TABLE` of a foreign key's parent does not move the key's reference
  in kilit's model (older than this behavior): `RENAME TABLE users TO people;
  ALTER TABLE people MODIFY id INT ...` gets a measured answer where MySQL
  fails with 3780.
- A trigger or a stored routine already on the server can fill a new table
  with rows that no migration shows.
- The rows a table holds, and the history of its instant column changes, are
  not in a schema. kilit prints the situations that depend on them as notes.

## Sessions and several files

- Several input files are several sessions, as `mysql < a.sql` and
  `mysql < b.sql` are. `foreign_key_checks`, `sql_log_bin` and
  `old_alter_table` start from the MySQL defaults in each file, and a note
  names the earlier file's `SET`.
- After a `SET GLOBAL` or `SET PERSIST` in an earlier file, the setting is
  unknown in the later ones. The tables carry over, except temporary tables.
- A `--pretend` file is one session.

## The metadata lock warning

- "Writes continue" describes the change itself. Every `ALTER TABLE` also
  waits for the metadata lock first. kilit warns about it, but cannot know
  which transactions will be open when you deploy.
- The warning covers `ALTER TABLE`, `CREATE INDEX`, `DROP INDEX ... ON`,
  `DROP TABLE`, `RENAME TABLE` (the old names) and `TRUNCATE TABLE`, on tables
  that exist before the input. The lab measured each of these.
- `LOCK TABLES` gets no warning. `LOCK TABLES ... READ` does not wait for a
  transaction that only read the table, and reads do not queue behind it when
  it waits.
- A statement kilit cannot read gets the warning only in the forms above:
  `ALTER`, `CREATE INDEX` and `DROP INDEX ... ON`, `DROP TABLES` and
  `RENAME TABLES`.
- `DROP TEMPORARY TABLE` names nothing. A temporary table created in one input
  file still hides the warning for a `DROP`, `TRUNCATE` or `ALTER` of the same
  name in a later file.
- A table made by `CREATE TABLE ... LIKE` or `AS SELECT` is treated as existing,
  so it is named. A `DROP`, `RENAME` or `TRUNCATE` of a name dropped earlier in
  the input names it too, although MySQL then gives error 1146 or 1051.

## Laravel

- On Windows, the marks that PowerShell leaves in the double-byte code pages
  (932, 936, 949, 950) and in 869 and 874 are not known.
- `--pretend` does not print every read: `DB::getPdo()->query()`, Laravel 10's
  Doctrine introspection and the affected-row counts of writes are not seen.
  A `with ... update` gets the read note too.
- An error raised outside any migration, after a migration that printed no
  statements, is blamed on that migration.
- A class-named migration whose name is wider than the terminal has no dots
  and is unreadable; its statements go under the migration before it.

More on these in [the Laravel notes](laravel.md#what-kilit-does-not-see).
