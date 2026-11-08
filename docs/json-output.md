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

`format_version` is `"1.0"`, a string, and it is the first key. A minor
increase (`"1.1"`) adds fields. A major increase (`"2.0"`) removes, renames or
changes one. Ignore fields you do not know, and reject a major version you do
not support.

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

Each statement always has `line`, `sql` and `impact` (`fails`, `writes_wait`,
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

A statement whose impact is `unknown` may carry `case`, `algorithm`,
`evidence` and `notes` too. An example is a statement that forces
`ALGORITHM=COPY` after `SET old_alter_table = ON`: the lab measured the copy,
but not whether writes wait, so `writes` is `unknown`.

## Warnings

A warning has `id`, `tables` and `message`, and `file` on a `laravel-error`.
The ids, in the order they appear in the array:

| Id | When | `tables` |
| --- | --- | --- |
| `server-mismatch` | the schema dump names another MySQL series, or MariaDB with `--server`, or a version kilit cannot read | empty |
| `laravel-error` | Laravel stopped with an error; `file` names the input | empty |
| `metadata-lock` | the input alters a table that exists before it | the altered tables |

The `metadata-lock` warning is left out when no existing table is altered. A
new table, and a statement that is not checked, do not count.

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
  "format_version": "1.0",
  "kilit": "0.0.0",
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
            "add-column-current-timestamp"
          ],
          "notes": [
            "if the table has had 64 instant column changes since it was last rebuilt: INPLACE, rebuilds the table, writes continue (add-column-after-64-changes)"
          ]
        },
        {
          "line": 2,
          "sql": "alter table `orders` add index `orders_amount_index` (`amount`)",
          "impact": "writes_continue",
          "case": "add-index",
          "algorithm": "INPLACE",
          "rebuilds_table": false,
          "copies_rows": false,
          "writes": "allowed",
          "evidence": [
            "add-index"
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
      "message": "Every ALTER TABLE first waits for the open transactions that have used its table to end. While it waits, new queries on that table queue behind it, reads too, even for an INSTANT change, and by default MySQL lets it wait a year (lock_wait_timeout = 31536000). Run SET SESSION lock_wait_timeout = 5 before the migration to cap the wait: the ALTER then fails with error 1205 after 5 seconds, the queries behind it go on, and you can run the migration again later."
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
