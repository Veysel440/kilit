# The GitHub Action

The Action `Veysel440/kilit` installs kilit in a GitHub Actions job. This page
covers what it does, how to pin it, and a complete workflow that checks the
migrations of a Laravel pull request.

- [What the Action does](#what-the-action-does)
- [Pinning](#pinning)
- [A workflow for a Laravel pull request](#a-workflow-for-a-laravel-pull-request)
- [Why each setting matters](#why-each-setting-matters)
- [The dump must be current](#the-dump-must-be-current)

## What the Action does

The Action, named *Install kilit*, downloads the archive of a kilit release
for the runner, checks it against the release's `SHA256SUMS`, unpacks it and
puts the folder with `kilit` on the `PATH` for the steps after it. If the
checksum does not match, nothing is unpacked and the step fails.

It does not run kilit, and it does not check anything: your workflow does, in
the next step. It does not build kilit and does not need a Rust toolchain.

Its one input is `version`, a release such as `0.1.0`: three numbers and two
dots, nothing else. The default is the release the Action's tag belongs to, so
`Veysel440/kilit@v0.1.0` installs kilit 0.1.0. To install another release, set
the input:

```yaml
- uses: Veysel440/kilit@v0.1.0
  with:
    version: 0.1.0
```

It runs on Linux (X64 and ARM64), macOS (X64 and ARM64) and Windows (X64)
runners. On any other runner it fails and says so.

## Pinning

`Veysel440/kilit@v0.1.0` names a tag. To pin the Action to the commit the tag
points at, write the commit and keep the tag as a comment:

```yaml
- uses: Veysel440/kilit@<commit sha> # v0.1.0
```

Find the commit with `gh api repos/Veysel440/kilit/commits/v0.1.0 --jq .sha`.

## A workflow for a Laravel pull request

This workflow loads production's schema dump into a MySQL service, runs
`php artisan migrate --pretend` against it, and pipes the output into kilit. It
was run as written on Laravel 12 and 13, against MySQL 8.4.13 with `--server
mysql-8.4` and against MySQL 8.0.46 with `--server mysql-8.0`: only the pending
migration was printed. Put it in `.github/workflows/kilit.yml`:

```yaml
name: kilit

on: pull_request

permissions:
  contents: read

jobs:
  migrations:
    runs-on: ubuntu-24.04
    services:
      mysql:
        image: mysql:8.4
        env:
          MYSQL_ROOT_PASSWORD: secret
          MYSQL_DATABASE: app
        ports:
          - 3306:3306
        options: >-
          --health-cmd "mysqladmin ping -h 127.0.0.1 -uroot -psecret"
          --health-interval 10s
          --health-timeout 5s
          --health-retries 5
    env:
      APP_ENV: testing
      DB_CONNECTION: mysql
      DB_HOST: 127.0.0.1
      DB_PORT: 3306
      DB_DATABASE: app
      DB_USERNAME: root
      DB_PASSWORD: secret
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: shivammathur/setup-php@eb7c497e18156a6bbabfef1d3a82760b9eda3962 # 2.40.0
        with:
          php-version: "8.3"
      - run: composer install --no-interaction --no-progress
      - uses: Veysel440/kilit@v0.1.0
      - name: Load the schema dump into MySQL
        env:
          MYSQL_CONTAINER: ${{ job.services.mysql.id }}
        run: docker exec -i "$MYSQL_CONTAINER" mysql -uroot -p"$DB_PASSWORD" "$DB_DATABASE" < database/schema/mysql-schema.sql
      - name: Check the pending migrations
        shell: bash
        run: php artisan migrate --pretend | kilit check --format github --server mysql-8.4 --schema database/schema/mysql-schema.sql
```

A third-party action, such as the PHP setup here, runs with your job's rights.
Pin it to a commit SHA, as the example does, and keep the version in a comment.

`--server` names the series of the MySQL service, `mysql-8.4` here. Use the
series of your production server, and the same image for the service
(`mysql:8.0` and `--server mysql-8.0` for 8.0); `kilit servers` lists the names.
Laravel's schema dump is written without comments, so it has no `-- Server
version` line, and kilit cannot warn you when the dump's server and the server
it checks against differ. Always pass `--server` yourself.

The health check connects over TCP with `-h 127.0.0.1`, on purpose. The image
starts a temporary server to set up the database. Over the socket the check
already passes while that server runs, and loading the dump then fails with
error 1045. Over TCP it passes only when the setup is done.

The dump is loaded with the client inside the service container, so the runner
needs no MySQL client. `shell: bash` runs the last step as `bash -eo
pipefail`, so if `php artisan` fails, the step fails too. Without it the step
uses `bash -e`, and only the exit status of kilit counts.

## Why each setting matters

All of these were measured.

- `APP_ENV: testing`. Without it Laravel takes the app for production and
  stops at its confirmation. kilit reports that like Laravel's error screen:
  Laravel stopped, exit status 2. No `.env` file and no `APP_KEY` are needed
  for `migrate --pretend`.
- `DB_CONNECTION: mysql`. A new Laravel app uses SQLite by default. Then
  `--pretend` prints every migration in SQLite's SQL, and kilit cannot read it
  as MySQL. On a fresh checkout `database/database.sqlite` does not exist, so
  Laravel first prints a warning that the SQLite database does not exist and
  asks "Would you like to create it?". Without a terminal it answers yes and
  creates the file. kilit cannot read the warning and the question, calls each
  statement unknown and exits 2. If the file already exists, because an earlier
  step created it or it is committed, Laravel prints no warning, every
  statement is unknown and kilit exits 0 by default, so the step passes while
  nothing was checked.
- `DB_DATABASE` set to the database of the service, and not empty. Without it
  Laravel uses the name `laravel`; with a name that does not exist, that name as
  written. In both cases it prints a warning that the database does not exist
  on the `mysql` connection and asks to create it. Without a terminal it
  answers yes, creates the database and its `migrations` table for real, and
  prints every migration as pending, `CREATE TABLE` statements included. kilit
  cannot read the warning and the question and exits 2. An empty `DB_DATABASE`
  also makes the step that loads the dump fail, with `ERROR 1046 (3D000) ... No
  database selected`.
- `DB_HOST: 127.0.0.1` and `DB_PORT: 3306`: the service is published on the
  runner's port 3306, not under a host name.

## The dump must be current

`migrate --pretend` prints the migrations that the database's `migrations`
table does not list as run. Laravel's dump holds those rows. If a migration
was deployed after the dump was written, the dump does not list it, it shows
as pending in `--pretend`, and kilit checks it again. It appears in the report;
it gets an annotation only if it fails or makes writes wait. Write the dump
again, from a database that has production's migrations applied, when
production moves on.

Laravel 13 prints a shorter form of `--pretend` output in some environments,
when it detects certain environment variables. kilit reads it in the same way.
