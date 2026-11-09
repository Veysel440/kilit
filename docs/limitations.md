# Limitations

What kilit does not do, and what it does instead. When it does not know, it
says `UNKNOWN` and prints the reason; it never reports an unmeasured change as
safe.

- [Input](#input)
- [What is measured](#what-is-measured)
- [The server kilit cannot see](#the-server-kilit-cannot-see)
- [Sessions and several files](#sessions-and-several-files)
- [The metadata lock warning](#the-metadata-lock-warning)
- [Laravel](#laravel)

## Input

- sqlparser 0.63 cannot read `CONVERT TO CHARACTER SET`, `RENAME INDEX`,
  `ALGORITHM=NOCOPY`, `OPTIMIZE TABLE` or `CREATE FULLTEXT INDEX`. Each is
  `UNKNOWN: kilit cannot read it`. A table named in such a statement is lost
  from then on.
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

- `MODIFY` and `CHANGE` need the table's current definition. Without a schema,
  kilit lists what the lab measured instead of answering.
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

## The server kilit cannot see

- kilit assumes the defaults listed in
  [the findings](findings.md#server-settings-the-lab-measured-with). It
  follows `foreign_key_checks` and `old_alter_table` when the input sets them.
  Other settings the input names make `UNKNOWN` the answers they could change;
  the findings say which. A server configured otherwise is invisible to it.
- A table with no `ENGINE` is taken as InnoDB, the server default. kilit does
  not read `default_storage_engine` from a `--schema` file, so a table created
  under a different default there is still taken as InnoDB.
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
