# JSON output

`kilit check --format json` prints the same answers as the text report, as one
JSON document. The text report is for people and may change in any release.
The JSON is the one to parse.

- [Versioning](#versioning)
- [Top level](#top-level)
- [Schema](#schema)
- [Sources and statements](#sources-and-statements)
- [Warnings](#warnings)
- [Example](#example)

## Versioning

`format_version` is `"1.1"`, a string, and it is the first key. A minor
increase adds fields: `"1.1"` added the `migration-output` warning and its
keys `migration` and `lines`, and the statement keys `table` and
`table_state`. A major increase (`"2.0"`) removes, renames or changes one.
Ignore fields you do not know, and reject a major version you do not support.

## Top level

The keys come in this order.

| Key | Holds |
| --- | --- |
| `format_version` | the version of this format |
| `kilit` | kilit's version |
| `server` | the exact release checked against, such as `mysql-8.4.13` |
| `schema` | `null` without `--schema`; else an object, see [Schema](#schema) |
| `sources` | one entry per migration, or per SQL file, see [Sources and statements](#sources-and-statements) |
| `unreadable` | lines of the input kilit does not understand: `input`, `line`, `text` |
| `warnings` | see [Warnings](#warnings) |
| `summary` | counts: `statements`, `fails`, `writes_wait`, `writes_continue`, `new_table`, `unknown`, `not_checked` |
| `fail_on` | what counted for the exit status, from `fails`, `waits`, `unknown` |
| `exit_code` | the exit status |

## Schema

| Key | Holds |
| --- | --- |
| `file` | the name of the `--schema` file |
| `tables` | the names of the tables it creates |
| `lost` | the tables kilit stopped trusting while it read the `--schema` file, each with `table` and `reason`; a table lost later, during the migration, is not listed |

## Sources and statements

Each entry of `sources` has `name`, `kind` (`laravel_migration` or `sql`) and
`statements`.

Each statement always has `line`, `sql`, `table`, `table_state` and `impact`
(`fails`, `writes_wait`, `writes_continue`, `new_table`, `unknown` or
`not_checked`). `table` is the name of the table the statement works on, as
written, or `null` for a statement that names none, such as a `SET`.
`table_state` says what kilit knew of that table when it read the statement,
or is `null` when `table` is:

| `table_state` | Meaning |
| --- | --- |
| `known` | kilit has the table, from the `--schema` file or from earlier statements, and the answer takes its definition into account |
| `created` | an earlier statement of the input created the table |
| `lost` | kilit had the table and stopped trusting it, for a reason the `reason` of the statement or the `lost` list of the schema gives |
| `unseen` | kilit has never seen the table: it is not in the dump, there is no dump, or the name is written another way than the table's |

On a `lost` or `unseen` table, a change that the table could make heavier is
`unknown`, with the likely answer in `reason`. When every table it may be
gives the same result, the impact is `writes_wait` (or `fails`) instead, with
`case`, `evidence` (the scenarios of the plain InnoDB case), `notes` (one per
situation), `reason` and, for `writes_wait`, `writes` set to `blocked`; there
is no `algorithm`, `rebuilds_table`, `copies_rows` or `error`, because the
table decides them. Today this is `ADD FULLTEXT`. The other keys come only when
there is something to say, and are left out otherwise:

| Key | Holds |
| --- | --- |
| `case` | the kind of change the lab measured |
| `algorithm` | `INSTANT`, `INPLACE` or `COPY` |
| `rebuilds_table` | `true` or `false` |
| `copies_rows` | `true` or `false` |
| `writes` | `allowed`, `blocked` or `unknown` |
| `error` | the MySQL error number, when the statement fails |
| `evidence` | the lab scenarios behind the answer |
| `notes` | what kilit worked out, and situations that change the answer; an `unknown` statement lists the measured situations that would change the likely answer |
| `reason` | why the statement is unknown or not checked, or why kilit does not know the table behind a `writes_wait` or `fails` |
| `other_servers` | where another measured server answers differently: `server`, `impact` and `answer`, the line the text report would print for that server |

A statement whose impact is `unknown` may carry `case`, `algorithm`,
`evidence` and `notes` too. An example is a statement that forces
`ALGORITHM=COPY` after `SET old_alter_table = ON`: the lab measured the copy,
but not whether writes wait, so `writes` is `unknown`.

## Warnings

A warning has `id`, `tables` and `message`. A `laravel-error` also has `file`,
and a `migration-output` has `migration` and `lines`. The ids, in the order
they appear in the array:

| Id | When | `tables` |
| --- | --- | --- |
| `server-mismatch` | the schema dump names another MySQL series, or MariaDB with `--server`, or a version kilit cannot read | empty |
| `migration-output` | a migration printed text of its own between its name and its statements; `migration` names it and `lines` lists the text as `line` and `text` | empty |
| `laravel-error` | Laravel stopped with an error; `file` names the input | empty |
| `metadata-lock` | the input alters, drops, renames or truncates a table that exists before it | those tables |

A `migration-output` warning exists for each migration that printed such lines.
The lines are read as text, not as SQL, and they do not change the exit status.
A control character in the text is shown as U+FFFD. A line that looks like SQL
stays in `unreadable`; see [Laravel](laravel.md#text-a-migration-prints-itself).

The `metadata-lock` warning is left out when no existing table is changed. A
table created earlier in the input does not count. A `DROP TABLE` counts,
although kilit does not check it.

## Example

Run from the directory of the example schema, with two statements on standard
input. The output is complete and unedited.

```sh
$ cd crates/kilit-core/examples
$ kilit check --format json --schema mysql-schema.sql <<'SQL'
alter table `orders` add `coupon` varchar(255) null;
alter table `orders` add index `orders_amount_index` (`amount`);
SQL
```

```json
{
  "format_version": "1.1",
  "kilit": "0.1.0",
  "server": "mysql-8.4.13",
  "schema": {
    "file": "mysql-schema.sql",
    "tables": [
      "migrations",
      "orders",
      "shops",
      "users"
    ],
    "lost": []
  },
  "sources": [
    {
      "name": "standard input",
      "kind": "sql",
      "statements": [
        {
          "line": 1,
          "sql": "alter table `orders` add `coupon` varchar(255) null",
          "table": "orders",
          "table_state": "known",
          "impact": "writes_continue",
          "case": "add-column",
          "algorithm": "INSTANT",
          "rebuilds_table": false,
          "copies_rows": false,
          "writes": "allowed",
          "evidence": [
            "add-nullable-column",
            "add-column-after",
            "add-column-first",
            "add-column-not-null",
            "add-column-not-null-default",
            "add-column-options",
            "add-column-current-timestamp",
            "on-delete-cascade-add-nullable-column",
            "on-delete-set-null-add-nullable-column",
            "cascade-parent-add-nullable-column"
          ],
          "notes": [
            "if the table has had 64 instant column changes since it was last rebuilt: INPLACE, rebuilds the table, writes continue (add-column-after-64-changes)"
          ]
        },
        {
          "line": 2,
          "sql": "alter table `orders` add index `orders_amount_index` (`amount`)",
          "table": "orders",
          "table_state": "known",
          "impact": "writes_continue",
          "case": "add-index",
          "algorithm": "INPLACE",
          "rebuilds_table": false,
          "copies_rows": false,
          "writes": "allowed",
          "evidence": [
            "add-index",
            "on-delete-cascade-add-index",
            "on-delete-set-null-add-index",
            "on-update-cascade-add-index",
            "on-update-set-null-add-index",
            "on-delete-and-update-cascade-add-index",
            "foreign-key-add-index",
            "on-delete-restrict-add-index",
            "on-delete-no-action-add-index",
            "cascade-parent-add-index",
            "set-null-parent-add-index"
          ]
        }
      ]
    }
  ],
  "unreadable": [],
  "warnings": [
    {
      "id": "metadata-lock",
      "tables": [
        "orders"
      ],
      "message": "Every ALTER TABLE, DROP TABLE, RENAME TABLE and TRUNCATE TABLE first waits for the open transactions that have used any of its tables to end. While it waits, new queries on those tables queue behind it, reads too, even for an INSTANT change, and by default MySQL lets it wait a year (lock_wait_timeout = 31536000). Run SET SESSION lock_wait_timeout = 5 before the migration to cap the wait: the statement then fails with error 1205 after 5 seconds, the queries behind it go on, and you can run the migration again later."
    }
  ],
  "summary": {
    "statements": 2,
    "fails": 0,
    "writes_wait": 0,
    "writes_continue": 2,
    "new_table": 0,
    "unknown": 0,
    "not_checked": 0
  },
  "fail_on": [
    "fails",
    "waits"
  ],
  "exit_code": 0
}
```
