# Laravel

kilit reads the output of `php artisan migrate --pretend` from Laravel 10 to
13. This page covers how to run it on a Laravel project, how the output is
read, and what kilit does when the output is not a list of statements.

- [Running kilit on a Laravel project](#running-kilit-on-a-laravel-project)
- [How the output is read](#how-the-output-is-read)
- [When Laravel stops with an error](#when-laravel-stops-with-an-error)
- [Reads under `--pretend`](#reads-under---pretend)
- [`->change()` since Laravel 11](#-change-since-laravel-11)
- [Windows and PowerShell](#windows-and-powershell)
- [What kilit does not see](#what-kilit-does-not-see)

## Running kilit on a Laravel project

```sh
php artisan schema:dump
php artisan migrate --pretend | kilit check --schema database/schema/mysql-schema.sql
```

`schema:dump` writes your tables to `database/schema/<connection>-schema.sql`,
which is `mysql-schema.sql` for a connection named `mysql`. It runs the
`mysqldump` client with `--skip-comments`, so the file has no `-- Server
version` line and kilit cannot compare the dump's server with the one it
checks against. If you do not want the file in `database/schema`, write it
elsewhere with `--path`.

Laravel loads that file before it runs migrations on an empty database. It
does so only when no migration has run yet, and never under `--pretend`.

Run both commands against a database that already has production's migrations
applied. The `migrations` table decides which migrations are pending: on a
fresh database `migrate --pretend` prints every migration, `CREATE TABLE`
statements included, and kilit takes the later `ALTER`s for changes to new
tables and exits 0. Load production's schema dump into the database with the
`mysql` client first; Laravel's dump holds the `migrations` rows, so
`--pretend` then prints only the pending migrations. `migrate --pretend`
creates the `migrations` table when the database has none, so do not point it
at a database you do not want to touch.

Without `--schema`, kilit still answers everything that does not depend on the
table, and says so where it does.

## How the output is read

kilit recognizes `migrate --pretend` output on its own. Laravel 10 to 13 print
statements with the same line format: the migration's name, then one line for
each statement, marked with `⇂`. Laravel 13 can print a shorter form,
depending on its environment; kilit reads both.

A line without dots counts as a migration name only in Laravel's file-name
form, `2026_11_01_000002_add_note_to_orders_table`. Laravel prints the
class name of a migration that has one, and kilit reads that form when the dots
are there. The statement mark is `⇂`, or `?`, `???` and the byte forms
PowerShell leaves in single-byte code pages.

A `SET FOREIGN_KEY_CHECKS=0;` line that Laravel prints around a migration is a
statement like any other: kilit follows it into the statements after it.

## When Laravel stops with an error

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
before it prints anything else, so the error screen is all there is.

kilit's exit status is 2, also without `--strict`. (Laravel itself exits with 1
on an error screen.) With several input files, the line starts with the file's
name, as in `migrations/b.txt: Laravel stopped with an error in <migration>;
...`. In JSON the message is an entry of `warnings` with the id
`laravel-error`, the input's name in `file` and no tables, before the
`metadata-lock` warning. See [the JSON notes](json-output.md#warnings).

## Reads under `--pretend`

Under `--pretend` every read returns nothing. A migration such as
`if (Schema::hasTable('orders')) { ... }` prints the `select` and none of the
statements it guards, or prints them where they would not run. kilit adds a
note to the first read of each migration: `select`, `show`, `describe`, `desc`,
`explain`, `with`, `table` or `values`. Answers and the exit status do not
change. In JSON the note is in that statement's `notes`. A later read in the
same migration gets a one-line `read (not checked)` entry.

## `->change()` since Laravel 11

Since Laravel 11, `->change()` replaces the whole column definition and
becomes `MODIFY`. A modifier the call does not repeat is dropped. In the
README's example, `$table->string('status', 30)->change()` prints `modify
`status` varchar(30) not null`, without the `default 'new'` the column had.
kilit compares the new definition with the column's current one, from your
`--schema` file, and says what changes. Without a schema it cannot, and lists
what the lab measured instead. See
[the findings](findings.md#changing-a-column-with--change).

## Windows and PowerShell

Pipe in WSL, Git Bash or `cmd`, which pass Laravel's output through as it is.
PowerShell before 7.4 re-encodes text in a pipe and with `>`: kilit still
reads the statement mark that this garbles, but text in other languages inside
your SQL can come through garbled. PowerShell 7.4 and later do not re-encode.

## What kilit does not see

- The marks that PowerShell leaves in the double-byte code pages (932, 936,
  949, 950) and in 869 and 874 are not known.
- `--pretend` does not print every read: `DB::getPdo()->query()`, Laravel 10's
  Doctrine introspection and the affected-row counts of writes are not seen.
  A `with ... update` gets the read note too.
- An error raised outside any migration, after a migration that printed no
  statements, is blamed on that migration.
- A class-named migration whose name is wider than the terminal has no dots
  and is unreadable; its statements go under the migration before it.
- Laravel's `--pretend` output turns every run of spaces into one, even inside
  quotes, so kilit sees `DEFAULT 'a  b'` as `DEFAULT 'a b'`.
- `Schema::table` with `$table->engine('MyISAM')` prints nothing: Laravel
  writes `engine = MyISAM` only in `Schema::create`.

The full list is in [limitations](limitations.md).
