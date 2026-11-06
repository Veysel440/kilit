//! The kilit command as a user runs it: arguments, standard input, output
//! and exit status.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// kilit-core's examples: a Laravel app's tables, and what
/// `php artisan migrate --pretend` printed for six migrations on them.
fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../kilit-core/examples")
        .join(name)
}

/// Runs kilit with `args`, feeding `stdin` to it.
fn kilit(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kilit"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // kilit may exit before it reads, say on a bad option: then the write
    // fails, and the exit status tells the rest.
    let _ = child.stdin.take().unwrap().write_all(stdin.as_bytes());
    child.wait_with_output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn laravel_demo(args: &[&str]) -> Output {
    let schema = example("mysql-schema.sql");
    let pretend = std::fs::read_to_string(example("laravel-pretend.txt")).unwrap();
    let mut all = vec!["check", "--schema", schema.to_str().unwrap()];
    all.extend(args);
    kilit(&all, &pretend)
}

#[test]
fn reads_laravel_s_pretend_output_from_standard_input() {
    let output = laravel_demo(&[]);
    let text = stdout(&output);
    assert!(
        text.contains(
            "2026_11_02_090100_widen_amount_on_orders_table
  alter table `orders` modify `amount` bigint not null
    COPY, copies every row, writes wait
"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "    - order_events is created earlier in these migrations: nothing else uses it yet, \
             so nothing waits for it\n"
        ),
        "{text}"
    );
    assert!(text.contains("\nmetadata lock on orders:\n"), "{text}");
    assert!(
        text.ends_with(
            "\n8 statements: 3 make writes wait, 1 unknown, 2 run while writes continue, \
             1 changes a table created here, 1 not checked\n"
        ),
        "{text}"
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
}

#[test]
fn json_is_one_document_with_the_same_answers() {
    let output = laravel_demo(&["--format", "json"]);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["server"], "mysql-8.4.13");
    assert_eq!(json["sources"].as_array().unwrap().len(), 6);

    let widen = &json["sources"][1];
    assert_eq!(
        widen["name"],
        "2026_11_02_090100_widen_amount_on_orders_table"
    );
    assert_eq!(widen["kind"], "laravel_migration");
    let statement = &widen["statements"][0];
    assert_eq!(statement["line"], 7);
    assert_eq!(statement["impact"], "writes_wait");
    assert_eq!(statement["case"], "change-integer-type");
    assert_eq!(statement["algorithm"], "COPY");
    assert_eq!(statement["copies_rows"], true);
    assert_eq!(statement["writes"], "blocked");

    assert_eq!(json["sources"][3]["statements"][1]["impact"], "new_table");
    assert_eq!(json["warnings"][0]["id"], "metadata-lock");
    assert_eq!(json["warnings"][0]["tables"], serde_json::json!(["orders"]));
    assert_eq!(json["summary"]["writes_wait"], 3);
    assert_eq!(json["exit_code"], 1);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn writes_that_continue_exit_with_0() {
    let output = kilit(
        &["check"],
        "alter table `orders` add `coupon` varchar(255) null;",
    );
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
}

#[test]
fn strict_also_fails_on_what_kilit_cannot_tell() {
    let sql = "alter table `orders` drop `legacy_code`, drop `legacy_flag`;";
    assert_eq!(kilit(&["check"], sql).status.code(), Some(0));
    assert_eq!(kilit(&["check", "--strict"], sql).status.code(), Some(1));
}

#[test]
fn what_kilit_cannot_tell_on_a_new_table_is_unknown() {
    // Nothing uses the new table, but MySQL 8.0.46 and 8.4.13 reject the
    // second statement with error 1064: NOT VALID is PostgreSQL's.
    let sql = "create table `order_tags` (`id` bigint unsigned not null primary key, \
               `kind` varchar(20) not null);
               alter table `order_tags` add constraint `order_tags_kind_check` \
               check (`kind` in ('gift', 'rush')) not valid;";
    let output = kilit(&["check"], sql);
    let text = stdout(&output);
    assert!(
        text.ends_with("\n2 statements: 1 unknown, 1 not checked\n"),
        "{text}"
    );
    assert!(
        !text.contains("created earlier in these migrations"),
        "{text}"
    );
    assert!(!text.contains("metadata lock on"), "{text}");
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert_eq!(kilit(&["check", "--strict"], sql).status.code(), Some(1));
}

#[test]
fn a_postgresql_style_migration_gets_no_measured_answer() {
    // Written in PostgreSQL's words and fed to kilit as a MySQL migration:
    // MySQL 8.0.46 and 8.4.13 reject both statements with error 1064.
    let schema = example("mysql-schema.sql");
    let sql = "alter table orders add constraint orders_user_id_fk \
               foreign key (user_id) references users (id) not valid;
               create index concurrently if not exists orders_amount_index on orders (amount);";
    let output = kilit(&["check", "--schema", schema.to_str().unwrap()], sql);
    let text = stdout(&output);
    for reason in [
        "    UNKNOWN: MySQL 8.0.46 and 8.4.13 do not accept NOT VALID\n",
        "    UNKNOWN: MySQL 8.0.46 and 8.4.13 do not accept CREATE INDEX CONCURRENTLY \
         or CREATE INDEX IF NOT EXISTS\n",
    ] {
        assert!(text.contains(reason), "{text}");
    }
    assert!(text.ends_with("\n2 statements: 2 unknown\n"), "{text}");
    // MySQL rejects them before it locks anything.
    assert!(!text.contains("metadata lock on"), "{text}");
    assert_eq!(output.status.code(), Some(0), "{text}");
    let strict = kilit(
        &["check", "--strict", "--schema", schema.to_str().unwrap()],
        sql,
    );
    assert_eq!(strict.status.code(), Some(1));
}

#[test]
fn indexes_on_the_table_change_the_answer() {
    // $table->dropColumn('status'), whose index goes with it; then
    // $table->fullText('note') and $table->fullText('legacy_code').
    let schema = example("mysql-schema.sql");
    let schema = schema.to_str().unwrap();
    let sql = "alter table `orders` drop `status`;
               alter table `orders` add fulltext `orders_note_fulltext`(`note`);
               alter table `orders` add fulltext `orders_legacy_code_fulltext`(`legacy_code`);";
    let output = kilit(&["check", "--schema", schema], sql);
    let text = stdout(&output);
    for answer in [
        "  line 1: alter table `orders` drop `status`\n\
         \x20   INPLACE, rebuilds the table, writes continue\n\
         \x20   measured as drop-column (8 lab scenarios)\n\
         \x20   - status is in the index orders_status_index\n",
        "    INPLACE, rebuilds the table, writes wait\n\
         \x20   measured as add-fulltext-index (1 lab scenario)\n\
         \x20 line 3:",
        "    INPLACE, no rebuild, writes wait\n\
         \x20   measured as add-fulltext-index (1 lab scenario)\n\
         \x20   - orders has the FULLTEXT index orders_note_fulltext\n",
    ] {
        assert!(text.contains(answer), "{text}");
    }
    assert!(
        text.ends_with("\n3 statements: 2 make writes wait, 1 runs while writes continue\n"),
        "{text}"
    );
    assert_eq!(output.status.code(), Some(1));

    let output = kilit(&["check", "--schema", schema, "--format", "json"], sql);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let statements = json["sources"][0]["statements"].as_array().unwrap();
    let answers: Vec<(&str, bool, &str)> = statements
        .iter()
        .map(|statement| {
            (
                statement["impact"].as_str().unwrap(),
                statement["rebuilds_table"].as_bool().unwrap(),
                statement["evidence"][0].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        answers,
        [
            ("writes_continue", true, "drop-column-indexed"),
            ("writes_wait", true, "add-fulltext-index"),
            ("writes_wait", false, "add-second-fulltext-index"),
        ]
    );
    assert_eq!(statements[0]["evidence"].as_array().unwrap().len(), 8);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn expression_defaults_and_a_functional_index_get_their_measured_answers() {
    // $table->uuid('token')->default(new Expression('(UUID())'));
    // $table->rawIndex('(lower(email))', 'users_lower_email_index');
    // $table->date('due_on')->useCurrent();
    let schema = example("mysql-schema.sql");
    let schema = schema.to_str().unwrap();
    let sql = "alter table `orders` add `token` char(36) not null default (UUID());
               alter table `users` add index `users_lower_email_index`((lower(email)));
               alter table `orders` add `due_on` date not null default (CURDATE());";
    for strict in [false, true] {
        let mut args = vec!["check", "--schema", schema];
        if strict {
            args.push("--strict");
        }
        let output = kilit(&args, sql);
        let text = stdout(&output);
        for answer in [
            "  line 1: alter table `orders` add `token` char(36) not null default (UUID())\n\
             \x20   fails with error 1674\n\
             \x20   measured as add-column-binlog-unsafe-default (2 lab scenarios)\n\
             \x20   - assumes the binary log is on, the MySQL default\n",
            "    INPLACE, no rebuild, writes wait\n\
             \x20   measured as add-functional-index (1 lab scenario)\n\
             \x20 line 3:",
            "    COPY, copies every row, writes wait\n\
             \x20   measured as add-column-expression-default (5 lab scenarios)\n\
             \x20   - assumes the binary log is on, the MySQL default\n",
        ] {
            assert!(text.contains(answer), "{text}");
        }
        assert!(
            text.ends_with("\n3 statements: 1 fails, 2 make writes wait\n"),
            "{text}"
        );
        // One fails and two make writes wait: 1, with --strict or without.
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));

        args.extend(["--format", "json"]);
        let output = kilit(&args, sql);
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let statements = json["sources"][0]["statements"].as_array().unwrap();
        let answers: Vec<(&str, &str)> = statements
            .iter()
            .map(|statement| {
                (
                    statement["impact"].as_str().unwrap(),
                    statement["case"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            answers,
            [
                ("fails", "add-column-binlog-unsafe-default"),
                ("writes_wait", "add-functional-index"),
                ("writes_wait", "add-column-expression-default"),
            ]
        );
        assert_eq!(statements[0]["error"], 1674);
        assert_eq!(
            statements[2]["notes"][0],
            "assumes the binary log is on, the MySQL default"
        );
        assert_eq!(output.status.code(), Some(1));
    }
}

#[test]
fn a_statement_that_fails_changes_no_table_on_either_server() {
    // The first change fails (user_id is in a foreign key), so the second
    // still sees BIGINT UNSIGNED; the index MySQL refuses to add instantly
    // is not there when amount is dropped.
    let schema = example("mysql-schema.sql");
    let schema = schema.to_str().unwrap();
    let sql = "alter table `orders` modify `user_id` int unsigned not null;
               alter table `orders` modify `user_id` bigint unsigned not null;
               alter table `orders` add index `orders_amount_index`(`amount`), algorithm=instant;
               alter table `orders` drop `amount`;";
    let answers = |server: Option<&str>| {
        let mut args = vec!["check", "--schema", schema, "--format", "json"];
        if let Some(server) = server {
            args.extend(["--server", server]);
        }
        let output = kilit(&args, sql);
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        json["sources"][0]["statements"].clone()
    };
    let default_server = answers(None);
    let impacts: Vec<(&str, &str)> = default_server
        .as_array()
        .unwrap()
        .iter()
        .map(|statement| {
            (
                statement["impact"].as_str().unwrap(),
                statement["case"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        impacts,
        [
            ("fails", "change-integer-type"),
            ("writes_continue", "modify-same-definition"),
            ("fails", "add-index"),
            ("writes_continue", "drop-column"),
        ]
    );
    assert_eq!(default_server[3]["algorithm"], "INSTANT");
    assert_eq!(answers(Some("mysql-8.0.46")), default_server);
}

#[test]
fn a_check_constraint_in_the_schema_keeps_its_columns() {
    // The form SHOW CREATE TABLE writes, in a schema dump. The first statement
    // fails, so the column and the constraint stay for the second.
    let schema = Path::new(env!("CARGO_TARGET_TMPDIR")).join("schema-with-a-check.sql");
    std::fs::write(
        &schema,
        "CREATE TABLE `orders` (
           `id` bigint unsigned NOT NULL AUTO_INCREMENT,
           `user_id` bigint unsigned NOT NULL,
           `amount` int NOT NULL,
           PRIMARY KEY (`id`),
           CONSTRAINT `orders_chk_1` CHECK (((`amount` + `user_id`) > 0))
         ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;",
    )
    .unwrap();
    let schema = schema.to_str().unwrap();
    let sql = "ALTER TABLE orders DROP COLUMN amount;
               ALTER TABLE orders RENAME COLUMN amount TO total;
               ALTER TABLE orders DROP CONSTRAINT orders_chk_1;
               ALTER TABLE orders DROP COLUMN amount;";
    let output = kilit(&["check", "--schema", schema, "--format", "json"], sql);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let statements = json["sources"][0]["statements"].as_array().unwrap();
    let answers: Vec<(&str, &str)> = statements
        .iter()
        .map(|statement| {
            (
                statement["impact"].as_str().unwrap(),
                statement["case"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        answers,
        [
            ("fails", "drop-column"),
            ("fails", "rename-column"),
            ("writes_continue", "drop-check-constraint"),
            ("writes_continue", "drop-column"),
        ]
    );
    assert_eq!(statements[0]["error"], 3959);
    assert_eq!(statements[1]["error"], 3959);

    let output = kilit(&["check", "--schema", schema], sql);
    let text = stdout(&output);
    assert!(
        text.contains(
            "  line 1: ALTER TABLE orders DROP COLUMN amount\n\
             \x20   fails with error 3959\n\
             \x20   measured as drop-column (2 lab scenarios)\n\
             \x20   - the CHECK constraint orders_chk_1 uses amount and user_id\n"
        ),
        "{text}"
    );
}

#[test]
fn a_table_created_here_is_never_in_the_metadata_lock_warning() {
    // The ALTER fails, but no transaction can be using order_notes yet, so it
    // waits for nothing before it fails.
    let schema = example("mysql-schema.sql");
    let output = kilit(
        &[
            "check",
            "--schema",
            schema.to_str().unwrap(),
            "--format",
            "json",
        ],
        "create table `order_notes` (`id` bigint unsigned not null auto_increment primary key, \
         `order_id` bigint unsigned not null);
         alter table `order_notes` add constraint `order_notes_order_id_foreign` \
         foreign key (`order_id`) references `orders` (`id`);
         alter table `order_notes` modify `order_id` int not null;",
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let impacts: Vec<&str> = json["sources"][0]["statements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|statement| statement["impact"].as_str().unwrap())
        .collect();
    assert_eq!(impacts, ["not_checked", "new_table", "fails"]);
    assert_eq!(json["warnings"], serde_json::json!([]));
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn files_are_checked_in_order_as_one_migration() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("files_in_order");
    std::fs::create_dir_all(&dir).unwrap();
    let create = dir.join("1_create.sql");
    let alter = dir.join("2_alter.sql");
    std::fs::write(
        &create,
        "CREATE TABLE notes (id INT PRIMARY KEY, body TEXT);",
    )
    .unwrap();
    std::fs::write(&alter, "\nALTER TABLE notes ADD FULLTEXT (body);").unwrap();

    let output = kilit(
        &["check", create.to_str().unwrap(), alter.to_str().unwrap()],
        "",
    );
    let text = stdout(&output);
    assert!(
        text.contains(&format!("{}\n  line 2: ALTER TABLE notes", alter.display())),
        "{text}"
    );
    // The FULLTEXT index makes writes wait, but nothing uses the new table.
    assert_eq!(output.status.code(), Some(0), "{text}");
}

#[test]
fn nothing_to_migrate_is_nothing_to_check() {
    let output = kilit(&["check"], "\n   INFO  Nothing to migrate.  \n\n");
    assert!(stdout(&output).ends_with("\nnothing to check: Laravel has no pending migration\n"));
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn lines_kilit_cannot_read_exit_with_2() {
    let output = kilit(
        &["check"],
        "   INFO  Running migrations.

  2026_11_02_090100_widen_amount_on_orders_table .............................

   ERROR  [2026_11_02_090100_widen_amount_on_orders_table] failed to dump queries.
",
    );
    assert!(
        stdout(&output).contains(
            "  standard input, line 5: ERROR  [2026_11_02_090100_widen_amount_on_orders_table] \
             failed to dump queries.\n"
        ),
        "{}",
        stdout(&output)
    );
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn an_unknown_server_exits_with_2() {
    let output = kilit(&["check", "--server", "mysql-5.7.44"], "select 1;");
    assert_eq!(
        stderr(&output),
        "kilit: no lab results for mysql-5.7.44; kilit has results for mysql-8.0.46, \
         mysql-8.4.13\n"
    );
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_missing_file_exits_with_2() {
    let output = kilit(&["check", "no-such-migration.sql"], "");
    assert!(stderr(&output).starts_with("kilit: no-such-migration.sql: "));
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_bad_option_exits_with_2() {
    assert_eq!(kilit(&["check", "--nope"], "").status.code(), Some(2));
}

#[test]
fn servers_lists_what_kilit_has_results_for() {
    let output = kilit(&["servers"], "");
    let text = stdout(&output);
    assert!(text.starts_with("mysql-8.0.46  "), "{text}");
    assert!(text.contains("\nmysql-8.4.13  "), "{text}");
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn the_readme_example_is_real_output() {
    // The README shows some migrations of the demo, run from a Laravel
    // project's root: every block it shows must be in the real output.
    let readme = include_str!("../../../README.md");
    let command = "$ php artisan migrate --pretend | kilit check --schema \
                   database/schema/mysql-schema.sql\n";
    let start = readme.find(command).expect("the README shows the command") + command.len();
    let shown = &readme[start..start + readme[start..].find("```").unwrap()];

    let project = Path::new(env!("CARGO_TARGET_TMPDIR")).join("laravel-project");
    std::fs::create_dir_all(project.join("database/schema")).unwrap();
    std::fs::copy(
        example("mysql-schema.sql"),
        project.join("database/schema/mysql-schema.sql"),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_kilit"))
        .args(["check", "--schema", "database/schema/mysql-schema.sql"])
        .current_dir(&project)
        .stdin(std::fs::File::open(example("laravel-pretend.txt")).unwrap())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut real = String::new();
    std::io::Read::read_to_string(child.stdout.as_mut().unwrap(), &mut real).unwrap();
    child.wait().unwrap();

    for block in shown.split("\n\n") {
        assert!(
            real.contains(block.trim_end()),
            "the README shows\n{block}\nbut kilit prints\n{real}"
        );
    }
}

// A foreign key to a parent column without an index, or with a column of
// another type: MySQL 8.0.46 and 8.4.13 refuse it, and not always with the
// same error.

/// What a migration adds to the example's `orders`: a string column, and
/// the foreign key from it to `users.name`, which has no index.
const FOREIGN_KEY_TO_NAME: &str = "alter table `orders` add `user_name` varchar(255) null;
alter table `orders` add constraint `orders_user_name_foreign` \
foreign key (`user_name`) references `users` (`name`);";

/// kilit's JSON for `sql` against the example's tables, and the exit status.
fn json_check(server: Option<&str>, strict: bool, sql: &str) -> (serde_json::Value, Option<i32>) {
    let schema = example("mysql-schema.sql");
    let mut args = vec![
        "check",
        "--schema",
        schema.to_str().unwrap(),
        "--format",
        "json",
    ];
    if let Some(server) = server {
        args.extend(["--server", server]);
    }
    if strict {
        args.push("--strict");
    }
    let output = kilit(&args, sql);
    let json = serde_json::from_slice(&output.stdout).unwrap();
    (json, output.status.code())
}

#[test]
fn a_foreign_key_to_a_column_without_an_index_fails_with_the_error_of_each_server() {
    // $table->string('user_name')->nullable();
    // $table->foreign('user_name')->references('name')->on('users');
    let schema = example("mysql-schema.sql");
    let schema = schema.to_str().unwrap();
    for (server, code, other) in [
        (Some("mysql-8.0.46"), 1822, 6125),
        (None, 6125, 1822),
        (Some("mysql-8.4.13"), 6125, 1822),
    ] {
        let mut args = vec!["check", "--schema", schema];
        if let Some(server) = server {
            args.extend(["--server", server]);
        }
        let output = kilit(&args, FOREIGN_KEY_TO_NAME);
        let text = stdout(&output);
        let name = server.unwrap_or("mysql-8.4.13");
        assert!(
            text.starts_with(&format!("checked against {name}, ")),
            "{text}"
        );
        assert!(
            text.contains(&format!("    fails with error {code}\n")),
            "{text}"
        );
        assert!(!text.contains(&format!("error {other}")), "{text}");
        assert!(
            text.ends_with("\n2 statements: 1 fails, 1 runs while writes continue\n"),
            "{text}"
        );
        assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
        // A failure is a failure with --strict too.
        args.push("--strict");
        assert_eq!(kilit(&args, FOREIGN_KEY_TO_NAME).status.code(), Some(1));

        let (json, status) = json_check(server, false, FOREIGN_KEY_TO_NAME);
        assert_eq!(json["server"], name);
        let statements = json["sources"][0]["statements"].as_array().unwrap();
        assert_eq!(statements[0]["impact"], "writes_continue");
        assert_eq!(statements[1]["impact"], "fails");
        assert_eq!(statements[1]["error"], code);
        assert_eq!(json["summary"]["fails"], 1);
        assert_eq!(json["summary"]["writes_continue"], 1);
        assert_eq!(json["exit_code"], 1);
        assert_eq!(status, Some(1));
        let (json, status) = json_check(server, true, FOREIGN_KEY_TO_NAME);
        assert_eq!(json["sources"][0]["statements"][1]["error"], code);
        assert_eq!(json["exit_code"], 1);
        assert_eq!(status, Some(1));
    }
}

#[test]
fn a_foreign_key_to_a_plain_index_is_accepted_by_mysql_80_and_fails_on_mysql_84() {
    // $table->index('name') on the parent first: 8.0.46 takes the key and
    // copies the table, 8.4.13 refuses it with 6125.
    let sql =
        format!("alter table `users` add index `users_name_index`(`name`);\n{FOREIGN_KEY_TO_NAME}");
    let (json, status) = json_check(Some("mysql-8.0.46"), false, &sql);
    let statement = &json["sources"][0]["statements"][2];
    assert_eq!(statement["impact"], "writes_wait");
    assert_eq!(statement["algorithm"], "COPY");
    assert_eq!(statement["copies_rows"], true);
    assert_eq!(json["summary"]["fails"], 0);
    assert_eq!(status, Some(1));

    let (json, status) = json_check(None, false, &sql);
    let statement = &json["sources"][0]["statements"][2];
    assert_eq!(statement["impact"], "fails");
    assert_eq!(statement["error"], 6125);
    assert_eq!(json["summary"]["fails"], 1);
    assert_eq!(status, Some(1));
}

#[test]
fn an_integer_foreign_key_column_fails_with_3780_on_both_servers() {
    // $table->integer('user_id'); $table->foreign('user_id')->references('id')->on('users');
    // in a migration that creates the table.
    let sql = "create table `order_refunds` (`id` bigint unsigned not null auto_increment \
               primary key, `user_id` int not null);
               alter table `order_refunds` add constraint `order_refunds_user_id_foreign` \
               foreign key (`user_id`) references `users` (`id`);";
    for server in [Some("mysql-8.0.46"), None] {
        for strict in [false, true] {
            let (json, status) = json_check(server, strict, sql);
            let statements = json["sources"][0]["statements"].as_array().unwrap();
            assert_eq!(statements[0]["impact"], "not_checked");
            assert_eq!(statements[1]["impact"], "fails");
            assert_eq!(statements[1]["error"], 3780);
            assert_eq!(json["exit_code"], 1);
            assert_eq!(status, Some(1));
        }
    }
}

#[test]
fn restrict_fk_on_non_standard_key_makes_the_index_answer_unknown_and_strict_exits_with_1() {
    let sql = format!("SET restrict_fk_on_non_standard_key = OFF;\n{FOREIGN_KEY_TO_NAME}");
    for server in [Some("mysql-8.0.46"), None] {
        let (json, status) = json_check(server, false, &sql);
        let statements = json["sources"][0]["statements"].as_array().unwrap();
        assert_eq!(statements[2]["impact"], "unknown");
        assert_eq!(json["summary"]["unknown"], 1);
        assert_eq!(json["summary"]["fails"], 0);
        // Unknown alone does not fail the run, only --strict does.
        assert_eq!(json["exit_code"], 0);
        assert_eq!(status, Some(0));
        let (json, status) = json_check(server, true, &sql);
        assert_eq!(json["exit_code"], 1);
        assert_eq!(status, Some(1));
    }
}

#[test]
fn every_way_of_setting_restrict_fk_on_non_standard_key_is_unknown_for_a_foreign_key_to_a_plain_index()
 {
    let sql = |set: &str| {
        format!(
            "alter table `users` add index `users_name_index`(`name`);
             {set};
             {FOREIGN_KEY_TO_NAME}"
        )
    };
    for set in [
        "SET restrict_fk_on_non_standard_key := OFF",
        "SET PERSIST restrict_fk_on_non_standard_key = 0",
        "SET @@persist.restrict_fk_on_non_standard_key = 0",
    ] {
        for server in [Some("mysql-8.0.46"), None] {
            let (json, status) = json_check(server, false, &sql(set));
            let statements = json["sources"][0]["statements"].as_array().unwrap();
            assert_eq!(statements[3]["impact"], "unknown", "{set}");
            assert_eq!(status, Some(0), "{set}");
            assert_eq!(json_check(server, true, &sql(set)).1, Some(1), "{set}");
        }
    }
}

#[test]
fn null_on_delete_on_a_not_null_column_is_unknown_and_strict_exits_with_1() {
    // $table->foreignId('user_id')->constrained()->nullOnDelete(); on a column
    // that is not nullable.
    let sql = "alter table `orders` add `store_id` bigint unsigned not null;
               alter table `orders` add constraint `orders_store_id_foreign` \
               foreign key (`store_id`) references `users` (`id`) on delete set null;";
    for server in [Some("mysql-8.0.46"), None] {
        let (json, status) = json_check(server, false, sql);
        assert_eq!(json["sources"][0]["statements"][1]["impact"], "unknown");
        assert_eq!(status, Some(0));
        assert_eq!(json_check(server, true, sql).1, Some(1));
    }
}

// Step 9a: the storage engine. A table that mysqldump writes with
// `ENGINE=MyISAM` gets the answers the lab measured on MyISAM tables.

/// A mysqldump schema where `orders` is MyISAM and `users` is InnoDB.
fn write_schema_with_engine(name: &str, engine: &str) -> PathBuf {
    let file = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(
        &file,
        format!(
            "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(100) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;

CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `amount` int NOT NULL,
  `email` varchar(255) DEFAULT NULL,
  `note` varchar(50) DEFAULT NULL,
  PRIMARY KEY (`id`)
) ENGINE={engine} AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
"
        ),
    )
    .unwrap();
    file
}

/// kilit's JSON for `sql` against `schema`, and the exit status.
fn json_against(schema: &Path, strict: bool, sql: &str) -> (serde_json::Value, Option<i32>) {
    let mut args = vec![
        "check",
        "--schema",
        schema.to_str().unwrap(),
        "--format",
        "json",
    ];
    if strict {
        args.push("--strict");
    }
    let output = kilit(&args, sql);
    let json = serde_json::from_slice(&output.stdout).unwrap();
    (json, output.status.code())
}

const MYISAM_COPY_SQL: &str = "alter table orders add coupon varchar(255) null;";
const MYISAM_INSTANT_SQL: &str = "alter table orders rename column note to memo;";
const MYISAM_FAILS_SQL: &str = "alter table orders add unique index uq_email (email);";
const MYISAM_UNKNOWN_SQL: &str = "alter table orders modify amount smallint not null;";

#[test]
fn a_myisam_table_in_a_mysqldump_schema_gets_the_myisam_answers_in_json() {
    let schema = write_schema_with_engine("schema-with-a-myisam-table.sql", "MyISAM");
    let sql = format!(
        "{MYISAM_COPY_SQL}
         {MYISAM_INSTANT_SQL}
         {MYISAM_FAILS_SQL}
         alter table orders add constraint fk_orders_amount \
         foreign key (amount) references users (id);
         {MYISAM_UNKNOWN_SQL}"
    );
    let (json, status) = json_against(&schema, false, &sql);
    // The table is known, not lost.
    assert_eq!(
        json["schema"]["tables"],
        serde_json::json!(["orders", "users"])
    );
    assert_eq!(json["schema"]["lost"], serde_json::json!([]));
    let statements = json["sources"][0]["statements"].as_array().unwrap();
    assert_eq!(statements.len(), 5);

    // ADD COLUMN: COPY, writes wait.
    assert_eq!(statements[0]["impact"], "writes_wait");
    assert_eq!(statements[0]["case"], "myisam-add-column");
    assert_eq!(statements[0]["algorithm"], "COPY");
    assert_eq!(statements[0]["copies_rows"], true);
    assert_eq!(statements[0]["writes"], "blocked");

    // RENAME COLUMN: metadata only, writes continue.
    assert_eq!(statements[1]["impact"], "writes_continue");
    assert_eq!(statements[1]["case"], "myisam-rename-column");
    assert_eq!(statements[1]["algorithm"], "INSTANT");
    assert_eq!(statements[1]["copies_rows"], false);

    // A UNIQUE index on email, 1020 bytes in utf8mb4: 1071.
    assert_eq!(statements[2]["impact"], "fails");
    assert_eq!(statements[2]["error"], 1071);

    // INT against BIGINT UNSIGNED: MySQL accepts it on a MyISAM table, it is
    // 3780 on InnoDB.
    assert_eq!(statements[3]["impact"], "writes_wait");
    assert_eq!(statements[3]["case"], "myisam-add-foreign-key");
    assert!(statements[3]["error"].is_null(), "{}", statements[3]);

    // MODIFY INT to SMALLINT: not measured on MyISAM.
    assert_eq!(statements[4]["impact"], "unknown");
    assert!(
        statements[4]["reason"].as_str().unwrap().contains("MyISAM"),
        "{}",
        statements[4]
    );

    assert_eq!(json["summary"]["fails"], 1);
    assert_eq!(json["summary"]["writes_wait"], 2);
    assert_eq!(json["summary"]["writes_continue"], 1);
    assert_eq!(json["summary"]["unknown"], 1);
    assert_eq!(json["exit_code"], 1);
    assert_eq!(status, Some(1));
}

#[test]
fn a_myisam_table_in_a_mysqldump_schema_gets_the_myisam_answers_in_text() {
    let schema = write_schema_with_engine("schema-with-a-myisam-table-text.sql", "MyISAM");
    let sql = format!("{MYISAM_COPY_SQL}\n{MYISAM_INSTANT_SQL}\n{MYISAM_FAILS_SQL}");
    let output = kilit(&["check", "--schema", schema.to_str().unwrap()], &sql);
    let text = stdout(&output);
    assert!(
        text.contains(
            "  line 1: alter table orders add coupon varchar(255) null\n\
             \x20   COPY, copies every row, writes wait\n\
             \x20   measured as myisam-add-column ("
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "  line 2: alter table orders rename column note to memo\n\
             \x20   INSTANT, no rebuild, writes continue\n\
             \x20   measured as myisam-rename-column ("
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "  line 3: alter table orders add unique index uq_email (email)\n\
             \x20   fails with error 1071\n"
        ),
        "{text}"
    );
    assert!(!text.contains("could not follow"), "{text}");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
}

#[test]
fn the_exit_code_for_a_myisam_table_follows_the_impact_with_and_without_strict() {
    let schema = write_schema_with_engine("schema-with-a-myisam-table-exit.sql", "MyISAM");
    // (statement, exit status without --strict, with --strict)
    for (sql, plain, strict) in [
        // COPY: writes wait.
        (MYISAM_COPY_SQL, 1, 1),
        // A measured failure.
        (MYISAM_FAILS_SQL, 1, 1),
        // Metadata only: nothing waits.
        (MYISAM_INSTANT_SQL, 0, 0),
        // Not measured on MyISAM: only --strict fails the run.
        (MYISAM_UNKNOWN_SQL, 0, 1),
    ] {
        assert_eq!(json_against(&schema, false, sql).1, Some(plain), "{sql}");
        let (json, status) = json_against(&schema, true, sql);
        assert_eq!(status, Some(strict), "{sql}");
        assert_eq!(json["exit_code"], strict, "{sql}");
    }
}

#[test]
fn the_same_schema_with_an_innodb_orders_keeps_the_innodb_answers() {
    let schema = write_schema_with_engine("schema-with-an-innodb-table.sql", "InnoDB");
    let sql = format!(
        "{MYISAM_COPY_SQL}
         alter table orders add constraint fk_orders_amount \
         foreign key (amount) references users (id);"
    );
    let (json, status) = json_against(&schema, false, &sql);
    let statements = json["sources"][0]["statements"].as_array().unwrap();
    assert_eq!(statements[0]["impact"], "writes_continue");
    assert_eq!(statements[0]["case"], "add-column");
    assert_eq!(statements[0]["algorithm"], "INSTANT");
    assert_eq!(statements[1]["impact"], "fails");
    assert_eq!(statements[1]["error"], 3780);
    assert_eq!(status, Some(1));
}

#[test]
fn a_table_of_another_engine_is_unknown_and_strict_exits_with_1() {
    for engine in ["MEMORY", "ARCHIVE"] {
        let schema = write_schema_with_engine("schema-with-another-engine.sql", engine);
        let (json, status) = json_against(&schema, false, MYISAM_COPY_SQL);
        let statement = &json["sources"][0]["statements"][0];
        assert_eq!(statement["impact"], "unknown", "{engine}");
        assert!(
            statement["reason"].as_str().unwrap().contains(engine),
            "{engine}: {statement}"
        );
        assert_eq!(status, Some(0), "{engine}");
        assert_eq!(
            json_against(&schema, true, MYISAM_COPY_SQL).1,
            Some(1),
            "{engine}"
        );
        let output = kilit(
            &["check", "--schema", schema.to_str().unwrap()],
            MYISAM_COPY_SQL,
        );
        let text = stdout(&output);
        assert!(text.contains("UNKNOWN: "), "{engine}: {text}");
        assert!(text.contains(engine), "{engine}: {text}");
    }
}

#[test]
fn a_myisam_table_created_in_the_input_is_never_waited_for_unless_it_fails() {
    // Nothing uses the new table, so a measured success is new_table. A
    // measured failure still fails, and what is not measured is unknown.
    let sql = "create table `order_tags` (`id` bigint unsigned not null primary key, \
               `tag` varchar(255) not null, `amount` int not null) \
               engine = MyISAM default character set utf8mb4;
               alter table `order_tags` add `weight` int null;
               alter table `order_tags` add index `order_tags_tag_index`(`tag`);
               alter table `order_tags` modify `amount` smallint not null;";
    let output = kilit(&["check", "--format", "json"], sql);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let statements = json["sources"][0]["statements"].as_array().unwrap();
    assert_eq!(statements[0]["impact"], "not_checked");
    assert_eq!(statements[1]["impact"], "new_table");
    assert_eq!(statements[2]["impact"], "fails");
    assert_eq!(statements[2]["error"], 1071);
    assert_eq!(statements[3]["impact"], "unknown");
    assert_eq!(json["exit_code"], 1);
    assert_eq!(output.status.code(), Some(1));

    // Only the measured success and the unknown: exit 0, and 1 with --strict.
    let sql = "create table `order_tags` (`id` bigint unsigned not null primary key, \
               `amount` int not null) engine = MyISAM;
               alter table `order_tags` add `weight` int null;
               alter table `order_tags` modify `amount` smallint not null;";
    assert_eq!(kilit(&["check"], sql).status.code(), Some(0));
    assert_eq!(kilit(&["check", "--strict"], sql).status.code(), Some(1));
}

// Regression test for what the step 9a review found: a key over 1000 bytes
// on a MyISAM table is 1071 in a strict SQL mode and accepted, cut to a
// prefix, with sql_mode = ''. kilit does not follow the mode.

#[test]
fn a_myisam_key_over_1000_bytes_is_unknown_after_a_sql_mode_and_says_it_assumes_strict_mode_without_one()
 {
    let schema = Path::new(env!("CARGO_TARGET_TMPDIR")).join("schema-with-a-long-myisam-key.sql");
    std::fs::write(
        &schema,
        "CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `amount` int NOT NULL,
  `code` varchar(251) DEFAULT NULL,
  PRIMARY KEY (`id`)
) ENGINE=MyISAM DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
",
    )
    .unwrap();
    let index = "ALTER TABLE orders ADD INDEX idx_code (code);";

    // Without a sql_mode in the input: fails with 1071, and says what it
    // assumes.
    let (json, status) = json_against(&schema, false, index);
    let statement = &json["sources"][0]["statements"][0];
    assert_eq!(statement["impact"], "fails");
    assert_eq!(statement["error"], 1071);
    assert!(
        statement["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("strict SQL mode")),
        "{statement}"
    );
    assert_eq!(status, Some(1));

    // After a SET of sql_mode: unknown, which fails the run only with
    // --strict.
    let sql = format!("SET SESSION sql_mode = '';\n{index}");
    let (json, status) = json_against(&schema, false, &sql);
    assert_eq!(json["sources"][0]["statements"][1]["impact"], "unknown");
    assert_eq!(status, Some(0));
    assert_eq!(json_against(&schema, true, &sql).1, Some(1));
}
