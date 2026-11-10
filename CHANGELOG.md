# Changelog

All notable changes to kilit are listed here, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.1.0] - 2026-11-10

### Added

- `kilit check` reads SQL files or standard input, the output of `php artisan
  migrate --pretend` from Laravel 10 to 13 (including the shorter form Laravel
  13 prints when it detects certain environment variables), and schema dumps
  from `php artisan schema:dump` or `mysqldump --no-data` with `--schema`.
- For each statement kilit says what MySQL does: INSTANT, INPLACE (with or
  without a rebuild) or COPY, whether writes wait, or the error it fails with.
  Every answer comes from what kilit-lab measured: 240 scenarios, each run six
  ways, on MySQL 8.0.46 and 8.4.13, bound to 77 cases.
- A statement kilit has not measured, or cannot read, is UNKNOWN with the
  reason. A `CREATE TABLE`, a `DROP TABLE`, a `SET`, a read or a write of rows
  is not checked.
- With a schema, kilit follows your tables through the migration, so it can
  judge `->change()` (`MODIFY` and `CHANGE`) and the conditions that depend on
  the table: indexes, foreign keys, CHECK constraints, generated columns, the
  engine and the row format. It follows `foreign_key_checks`, `sql_log_bin` and
  `old_alter_table` when the input sets them.
- When another measured server answers a statement differently, a note says so;
  it never changes the exit status.
- One warning about the metadata lock, naming the tables, for every
  `ALTER TABLE`, `DROP TABLE`, `RENAME TABLE`, `TRUNCATE TABLE`, `CREATE INDEX`
  and `DROP INDEX` that touches a table that exists.
- Laravel stopping with an error, including its confirmation when the app takes
  itself for production, is reported as Laravel stopping, with exit status 2.
- Exit status: 0 when nothing counted was found, 1 when a statement fails or
  makes writes wait, 2 when kilit could not check everything, 3 when the only
  counted statements are unknown. `--fail-on` says what counts (default
  `fails,waits`); `--strict` also counts unknown statements.
- `--server` takes a release (`mysql-8.4.13`) or a series (`mysql-8.4`,
  `mysql-8.0`); the default is `mysql-8.4`. `kilit servers` lists them.
- `--format json` prints one JSON document, `format_version` 1.0.
- `--format github` prints the text report, then one GitHub Actions annotation
  per finding.
- Prebuilt binaries for Linux (x86_64 and aarch64, static), macOS (aarch64 and
  x86_64) and Windows (x86_64), with SHA256SUMS and build provenance
  attestations.
- The GitHub Action `Veysel440/kilit`, which installs kilit from the release
  after checking the archive against SHA256SUMS.
