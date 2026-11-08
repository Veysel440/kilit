//! The kilit command as a user runs it: arguments, standard input, output
//! and exit status.

use std::collections::{HashMap, HashSet};
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
fn strict_also_counts_what_kilit_cannot_tell_and_exits_with_3() {
    let sql = "alter table `orders` drop `legacy_code`, drop `legacy_flag`;";
    assert_eq!(kilit(&["check"], sql).status.code(), Some(0));
    assert_eq!(kilit(&["check", "--strict"], sql).status.code(), Some(3));
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
        text.ends_with(
            "\n2 statements: 1 unknown, 1 not checked\n\
             1 unknown statement is not counted in the exit status; \
             --fail-on fails,waits,unknown counts it\n"
        ),
        "{text}"
    );
    assert!(
        !text.contains("created earlier in these migrations"),
        "{text}"
    );
    assert!(!text.contains("metadata lock on"), "{text}");
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert_eq!(kilit(&["check", "--strict"], sql).status.code(), Some(3));
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
    assert!(
        text.ends_with(
            "\n2 statements: 2 unknown\n\
             2 unknown statements are not counted in the exit status; \
             --fail-on fails,waits,unknown counts them\n"
        ),
        "{text}"
    );
    // MySQL rejects them before it locks anything.
    assert!(!text.contains("metadata lock on"), "{text}");
    assert_eq!(output.status.code(), Some(0), "{text}");
    let strict = kilit(
        &["check", "--strict", "--schema", schema.to_str().unwrap()],
        sql,
    );
    assert_eq!(strict.status.code(), Some(3));
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
             \x20   measured as add-column-binlog-unsafe-default (8 lab scenarios)\n\
             \x20   - assumes the binary log is on with binlog_format = ROW, the MySQL default\n",
            "    INPLACE, no rebuild, writes wait\n\
             \x20   measured as add-functional-index (1 lab scenario)\n\
             \x20 line 3:",
            "    COPY, copies every row, writes wait\n\
             \x20   measured as add-column-expression-default (5 lab scenarios)\n\
             \x20   - assumes the binary log is on with binlog_format = ROW, the MySQL default\n",
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
            "assumes the binary log is on with binlog_format = ROW, the MySQL default"
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
fn files_are_checked_in_order_and_the_tables_carry_over_from_one_file_to_the_next() {
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
    for (server, code, other_server, other) in [
        (Some("mysql-8.0.46"), 1822, "mysql-8.4.13", 6125),
        (None, 6125, "mysql-8.0.46", 1822),
        (Some("mysql-8.4.13"), 6125, "mysql-8.0.46", 1822),
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
        // The other measured server answers the same statement with its own
        // error, and the note is the last line of the statement.
        assert!(
            text.contains(&format!(
                "    - on {other_server}: fails with error {other}\n\nmetadata lock on"
            )),
            "{text}"
        );
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
        assert_eq!(
            statements[1]["other_servers"],
            serde_json::json!([{
                "server": other_server,
                "impact": "fails",
                "answer": format!("fails with error {other}"),
            }])
        );
        assert!(statements[0].get("other_servers").is_none());
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
fn restrict_fk_on_non_standard_key_makes_the_index_answer_unknown_and_strict_exits_with_3() {
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
        assert_eq!(json["exit_code"], 3);
        assert_eq!(status, Some(3));
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
            assert_eq!(json_check(server, true, &sql(set)).1, Some(3), "{set}");
        }
    }
}

#[test]
fn null_on_delete_on_a_not_null_column_is_unknown_and_strict_exits_with_3() {
    // $table->foreignId('user_id')->constrained()->nullOnDelete(); on a column
    // that is not nullable.
    let sql = "alter table `orders` add `store_id` bigint unsigned not null;
               alter table `orders` add constraint `orders_store_id_foreign` \
               foreign key (`store_id`) references `users` (`id`) on delete set null;";
    for server in [Some("mysql-8.0.46"), None] {
        let (json, status) = json_check(server, false, sql);
        assert_eq!(json["sources"][0]["statements"][1]["impact"], "unknown");
        assert_eq!(status, Some(0));
        assert_eq!(json_check(server, true, sql).1, Some(3));
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
        // Not measured on MyISAM: only --strict counts it, with exit status 3.
        (MYISAM_UNKNOWN_SQL, 0, 3),
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
fn a_table_of_another_engine_is_unknown_and_strict_exits_with_3() {
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
            Some(3),
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

    // Only the measured success and the unknown: exit 0, and 3 with --strict.
    let sql = "create table `order_tags` (`id` bigint unsigned not null primary key, \
               `amount` int not null) engine = MyISAM;
               alter table `order_tags` add `weight` int null;
               alter table `order_tags` modify `amount` smallint not null;";
    assert_eq!(kilit(&["check"], sql).status.code(), Some(0));
    assert_eq!(kilit(&["check", "--strict"], sql).status.code(), Some(3));
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

    // After a SET of sql_mode: unknown, which stops the run (exit 3) only
    // with --strict.
    let sql = format!("SET SESSION sql_mode = '';\n{index}");
    let (json, status) = json_against(&schema, false, &sql);
    assert_eq!(json["sources"][0]["statements"][1]["impact"], "unknown");
    assert_eq!(status, Some(0));
    assert_eq!(json_against(&schema, true, &sql).1, Some(3));
}

#[test]
fn uuid_short_sysdate_and_user_defaults_fail_with_1674_and_the_exit_code_says_so() {
    // The form Laravel's grammar produces for an Expression default:
    // $table->unsignedBigInteger('ref')->default(new Expression('(UUID_SHORT())'));
    let schema = example("mysql-schema.sql");
    let schema = schema.to_str().unwrap();
    let sql = "alter table `orders` add `ref` bigint unsigned not null default (UUID_SHORT());
               alter table `orders` add `placed_at` datetime null default (SYSDATE());
               alter table `orders` add `created_by` varchar(288) not null default (USER());";
    for server in ["mysql-8.0.46", "mysql-8.4.13"] {
        for strict in [false, true] {
            let mut args = vec!["check", "--schema", schema, "--server", server];
            if strict {
                args.push("--strict");
            }
            let output = kilit(&args, sql);
            let text = stdout(&output);
            for (line, statement) in [
                (
                    1,
                    "alter table `orders` add `ref` bigint unsigned not null default (UUID_SHORT())",
                ),
                (
                    2,
                    "alter table `orders` add `placed_at` datetime null default (SYSDATE())",
                ),
                (
                    3,
                    "alter table `orders` add `created_by` varchar(288) not null default (USER())",
                ),
            ] {
                assert!(
                    text.contains(&format!(
                        "  line {line}: {statement}\n    fails with error 1674\n    measured as \
                         add-column-binlog-unsafe-default ("
                    )),
                    "{server}: {text}"
                );
            }
            assert!(
                text.contains("    - assumes the binary log is on with binlog_format = ROW, the MySQL default\n"),
                "{text}"
            );
            assert!(text.ends_with("\n3 statements: 3 fail\n"), "{text}");
            // Three failures: 1, with --strict or without.
            assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));

            args.extend(["--format", "json"]);
            let output = kilit(&args, sql);
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let statements = json["sources"][0]["statements"].as_array().unwrap();
            assert_eq!(statements.len(), 3);
            for statement in statements {
                assert_eq!(statement["impact"], "fails", "{server}");
                assert_eq!(statement["case"], "add-column-binlog-unsafe-default");
                assert_eq!(statement["error"], 1674);
                assert_eq!(
                    statement["notes"][0],
                    "assumes the binary log is on with binlog_format = ROW, the MySQL default"
                );
            }
            assert_eq!(output.status.code(), Some(1));
        }
    }
}

#[test]
fn a_uuid_short_default_after_sql_log_bin_is_off_is_unknown_and_only_strict_exits_with_3() {
    let schema = example("mysql-schema.sql");
    let schema = schema.to_str().unwrap();
    let sql = "SET sql_log_bin = 0;
               alter table `orders` add `ref` bigint unsigned not null default (UUID_SHORT());";
    let output = kilit(&["check", "--schema", schema, "--format", "json"], sql);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let statement = &json["sources"][0]["statements"][1];
    assert_eq!(statement["impact"], "unknown");
    assert_eq!(
        statement["reason"],
        "a statement before this one sets sql_log_bin = 0, and the lab has measured expression \
         defaults only with the binary log on"
    );
    assert_eq!(output.status.code(), Some(0));
    let output = kilit(&["check", "--schema", schema, "--strict"], sql);
    assert_eq!(output.status.code(), Some(3), "{}", stdout(&output));
}

// Step 9b: a table's ROW_FORMAT and the old_alter_table setting.

/// A mysqldump schema with `users` and `orders` as the lab has them, where
/// `orders` is COMPRESSED, and a COMPACT table `logs`.
fn write_schema_with_row_formats(name: &str) -> PathBuf {
    let file = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(
        &file,
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
) ENGINE=InnoDB AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci ROW_FORMAT=COMPRESSED KEY_BLOCK_SIZE=8;

CREATE TABLE `logs` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `message` varchar(255) DEFAULT NULL,
  `level` int NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci ROW_FORMAT=COMPACT;
",
    )
    .unwrap();
    file
}

// (statement, exit status without --strict, exit status with --strict; 3 is
// an unknown statement counted by --strict)
const ROW_FORMAT_STATEMENTS: [(&str, i32, i32); 6] = [
    // COMPRESSED: INPLACE with a rebuild, writes continue (INSTANT on DYNAMIC).
    ("alter table orders add coupon varchar(255) null;", 0, 0),
    // COMPRESSED: a rename is metadata only.
    ("alter table orders rename column note to memo;", 0, 0),
    // COMPRESSED: INT to BIGINT copies the table.
    ("alter table orders modify amount bigint not null;", 1, 1),
    // COMPACT: 255 characters of utf8mb4 are 1020 bytes, the limit is 767.
    (
        "alter table logs add unique index uq_message (message);",
        1,
        1,
    ),
    // COMPACT: a new column is instant.
    ("alter table logs add coupon varchar(255) null;", 0, 0),
    // COMPRESSED: not measured, unknown.
    (
        "alter table orders add constraint fk_orders_user foreign key (user_id) \
         references users (id);",
        0,
        3,
    ),
];

#[test]
fn compressed_and_compact_tables_of_a_mysqldump_schema_get_their_own_answers_in_json() {
    let schema = write_schema_with_row_formats("schema-with-row-formats.sql");
    let sql: String = ROW_FORMAT_STATEMENTS
        .iter()
        .map(|(statement, _, _)| format!("{statement}\n"))
        .collect();
    let (json, status) = json_against(&schema, false, &sql);
    // The tables are known, not lost.
    assert_eq!(
        json["schema"]["tables"],
        serde_json::json!(["logs", "orders", "users"])
    );
    assert_eq!(json["schema"]["lost"], serde_json::json!([]));
    let statements = json["sources"][0]["statements"].as_array().unwrap();
    assert_eq!(statements.len(), 6);

    // ADD COLUMN on COMPRESSED: INPLACE, rebuilds the table, writes continue.
    assert_eq!(statements[0]["impact"], "writes_continue");
    assert_eq!(statements[0]["algorithm"], "INPLACE");
    assert_eq!(statements[0]["rebuilds_table"], true);
    assert_eq!(statements[0]["copies_rows"], false);

    // RENAME COLUMN: INSTANT.
    assert_eq!(statements[1]["impact"], "writes_continue");
    assert_eq!(statements[1]["algorithm"], "INSTANT");
    assert_eq!(statements[1]["rebuilds_table"], false);

    // INT to BIGINT: COPY, writes wait.
    assert_eq!(statements[2]["impact"], "writes_wait");
    assert_eq!(statements[2]["algorithm"], "COPY");
    assert_eq!(statements[2]["copies_rows"], true);
    assert_eq!(statements[2]["writes"], "blocked");

    // The 1020-byte unique index on COMPACT: 1071.
    assert_eq!(statements[3]["impact"], "fails");
    assert_eq!(statements[3]["error"], 1071);

    // ADD COLUMN on COMPACT: INSTANT.
    assert_eq!(statements[4]["impact"], "writes_continue");
    assert_eq!(statements[4]["algorithm"], "INSTANT");

    // A foreign key on COMPRESSED: not measured.
    assert_eq!(statements[5]["impact"], "unknown");
    assert!(statements[5]["reason"].is_string(), "{}", statements[5]);

    assert_eq!(json["summary"]["fails"], 1);
    assert_eq!(json["summary"]["writes_wait"], 1);
    assert_eq!(json["summary"]["writes_continue"], 3);
    assert_eq!(json["summary"]["unknown"], 1);
    assert_eq!(json["exit_code"], 1);
    assert_eq!(status, Some(1));
}

#[test]
fn compressed_and_compact_tables_of_a_mysqldump_schema_get_their_own_answers_in_text() {
    let schema = write_schema_with_row_formats("schema-with-row-formats-text.sql");
    let sql: String = ROW_FORMAT_STATEMENTS
        .iter()
        .map(|(statement, _, _)| format!("{statement}\n"))
        .collect();
    let output = kilit(&["check", "--schema", schema.to_str().unwrap()], &sql);
    let text = stdout(&output);
    for (line, statement, answer) in [
        (
            1,
            "alter table orders add coupon varchar(255) null",
            "INPLACE, rebuilds the table, writes continue",
        ),
        (
            2,
            "alter table orders rename column note to memo",
            "INSTANT, no rebuild, writes continue",
        ),
        (
            3,
            "alter table orders modify amount bigint not null",
            "COPY, copies every row, writes wait",
        ),
        (
            4,
            "alter table logs add unique index uq_message (message)",
            "fails with error 1071",
        ),
        (
            5,
            "alter table logs add coupon varchar(255) null",
            "INSTANT, no rebuild, writes continue",
        ),
    ] {
        assert!(
            text.contains(&format!("  line {line}: {statement}\n    {answer}\n")),
            "{answer}: {text}"
        );
    }
    assert!(text.contains("UNKNOWN: "), "{text}");
    assert!(!text.contains("could not follow"), "{text}");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
}

#[test]
fn the_exit_code_for_row_format_tables_follows_the_impact_with_and_without_strict() {
    let schema = write_schema_with_row_formats("schema-with-row-formats-exit.sql");
    for (sql, plain, strict) in ROW_FORMAT_STATEMENTS {
        assert_eq!(json_against(&schema, false, sql).1, Some(plain), "{sql}");
        let (json, status) = json_against(&schema, true, sql);
        assert_eq!(status, Some(strict), "{sql}");
        assert_eq!(json["exit_code"], strict, "{sql}");
    }
}

#[test]
fn the_same_schema_without_row_formats_keeps_the_dynamic_answers() {
    // Regression: the INNODB tables of the 9a schema have no ROW_FORMAT.
    let schema = write_schema_with_engine("schema-without-row-formats.sql", "InnoDB");
    let (json, status) = json_against(
        &schema,
        false,
        "alter table orders add coupon varchar(255) null;",
    );
    let statement = &json["sources"][0]["statements"][0];
    assert_eq!(statement["impact"], "writes_continue");
    assert_eq!(statement["algorithm"], "INSTANT");
    assert_eq!(status, Some(0));
}

#[test]
fn old_alter_table_on_makes_add_column_copy_the_table_and_the_exit_code_says_so() {
    // SET SESSION old_alter_table = ON: an ALTER without ALGORITHM copies
    // the table (lab: old-alter-table-add-nullable-column).
    let schema = write_schema_with_engine("schema-for-old-alter-table.sql", "InnoDB");
    let sql = "SET SESSION old_alter_table = ON;
               alter table orders add coupon varchar(255) null;";
    for server in ["mysql-8.0.46", "mysql-8.4.13"] {
        for strict in [false, true] {
            let mut args = vec![
                "check",
                "--schema",
                schema.to_str().unwrap(),
                "--server",
                server,
                "--format",
                "json",
            ];
            if strict {
                args.push("--strict");
            }
            let output = kilit(&args, sql);
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let statement = &json["sources"][0]["statements"][1];
            assert_eq!(statement["impact"], "writes_wait", "{server}");
            assert_eq!(statement["algorithm"], "COPY", "{server}");
            assert_eq!(statement["copies_rows"], true, "{server}");
            assert_eq!(statement["writes"], "blocked", "{server}");
            assert_eq!(json["exit_code"], 1, "{server}");
            assert_eq!(output.status.code(), Some(1), "{server}");
        }
    }
    let output = kilit(&["check", "--schema", schema.to_str().unwrap()], sql);
    let text = stdout(&output);
    assert!(
        text.contains(
            "  line 2: alter table orders add coupon varchar(255) null\n    COPY, copies every row, writes wait\n"
        ),
        "{text}"
    );
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
}

#[test]
fn the_same_alter_without_old_alter_table_is_instant_and_exits_with_0() {
    let schema = write_schema_with_engine("schema-for-old-alter-table-off.sql", "InnoDB");
    for sql in [
        "alter table orders add coupon varchar(255) null;",
        "SET SESSION old_alter_table = ON;
         SET SESSION old_alter_table = OFF;
         alter table orders add coupon varchar(255) null;",
    ] {
        let (json, status) = json_against(&schema, false, sql);
        let statements = json["sources"][0]["statements"].as_array().unwrap();
        let statement = statements.last().unwrap();
        assert_eq!(statement["impact"], "writes_continue", "{sql}");
        assert_eq!(statement["algorithm"], "INSTANT", "{sql}");
        assert_eq!(status, Some(0), "{sql}");
    }
}

#[test]
fn a_written_algorithm_wins_over_old_alter_table_in_the_command_too() {
    let schema = write_schema_with_engine("schema-for-old-alter-table-forced.sql", "InnoDB");
    let sql = "SET SESSION old_alter_table = ON;
               alter table orders add coupon varchar(255) null, algorithm=instant;";
    let (json, status) = json_against(&schema, false, sql);
    let statement = &json["sources"][0]["statements"][1];
    assert_eq!(statement["impact"], "writes_continue");
    assert_eq!(statement["algorithm"], "INSTANT");
    assert_eq!(status, Some(0));
}

#[test]
fn old_alter_table_set_globally_is_unknown_and_only_strict_exits_with_3() {
    let schema = write_schema_with_engine("schema-for-old-alter-table-global.sql", "InnoDB");
    let sql = "SET GLOBAL old_alter_table = ON;
               alter table orders add coupon varchar(255) null;";
    let (json, status) = json_against(&schema, false, sql);
    let statement = &json["sources"][0]["statements"][1];
    assert_eq!(statement["impact"], "unknown");
    assert!(
        statement["reason"]
            .as_str()
            .unwrap()
            .contains("old_alter_table"),
        "{statement}"
    );
    assert_eq!(json["exit_code"], 0);
    assert_eq!(status, Some(0));
    let (json, status) = json_against(&schema, true, sql);
    assert_eq!(json["exit_code"], 3);
    assert_eq!(status, Some(3));
}

// ---------------------------------------------------------------------------
// Laravel's error screen and migrations that read the database.
//
// The inputs are `php artisan migrate --pretend` runs of hand-written
// migrations on Laravel 12.71.0 and 13.40.0 (the short form is what 13.40.0
// prints where it detects an automated environment), with Collision (the
// default in a new application) and without it (Symfony's error screen),
// piped and in a terminal (CRLF line ends and colors, `_PTY`).
// ---------------------------------------------------------------------------

const C1_THROW_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000001_throw_after_alter_orders_table ...........................  \n",
    "\n",
    "   RuntimeException \n",
    "\n",
    "  boom from a migration\n",
    "\n",
    "  at database/migrations/step10/c1_throw/2026_02_01_000001_throw_after_alter_orders_table.php:15\n",
    "     11▕     {\n",
    "     12▕         Schema::table('orders', function (Blueprint $table) {\n",
    "     13▕             $table->integer('qty')->nullable();\n",
    "     14▕         });\n",
    "  ➜  15▕         throw new RuntimeException('boom from a migration');\n",
    "     16▕     }\n",
    "     17▕ };\n",
    "     18▕\n",
    "\n",
    "      \u{1b}[2m+27 vendor frames \u{1b}[22m\n",
    "\n",
    "  28  artisan:16\n",
    "      Illuminate\\Foundation\\Application::handleCommand(Object(Symfony\\Component\\Console\\Input\\ArgvInput))\n",
    "\n",
);

const C1_THROW_L12_PTY: &str = concat!(
    "\r\n",
    "  \u{1b}[37;44m INFO \u{1b}[39;49m Running migrations.  \r\n",
    "\r\n",
    "  2026_02_01_000001_throw_after_alter_orders_table \u{1b}[90m...........................\u{1b}[39m  \r\n",
    "\r\n",
    "  \u{1b}[41;1m RuntimeException \u{1b}[49;22m\r\n",
    "\r\n",
    "\u{1b}[39;1m  boom from a migration\u{1b}[39;22m\r\n",
    "\r\n",
    "  at \u{1b}[32mdatabase/migrations/step10/c1_throw/2026_02_01_000001_throw_after_alter_orders_table.php\u{1b}[39m:\u{1b}[32m15\u{1b}[39m\r\n",
    "    \u{1b}[90m 11\u{1b}[0m\u{1b}[90m▕ \u{1b}[0m\u{1b}[39;1m    \u{1b}[0m\u{1b}[35;1m{\u{1b}[0m\r\n",
    "    \u{1b}[90m 12\u{1b}[0m\u{1b}[90m▕ \u{1b}[0m\u{1b}[35;1m        \u{1b}[0m\u{1b}[39;1mSchema\u{1b}[0m\u{1b}[35;1m::\u{1b}[0m\u{1b}[39;1mtable\u{1b}[0m\u{1b}[35;1m(\u{1b}[0m\u{1b}[37m'orders'\u{1b}[0m\u{1b}[35;1m, function (\u{1b}[0m\u{1b}[39;1mBlueprint $table\u{1b}[0m\u{1b}[35;1m) {\u{1b}[0m\r\n",
    "    \u{1b}[90m 13\u{1b}[0m\u{1b}[90m▕ \u{1b}[0m\u{1b}[35;1m            \u{1b}[0m\u{1b}[39;1m$table\u{1b}[0m\u{1b}[35;1m->\u{1b}[0m\u{1b}[39;1minteger\u{1b}[0m\u{1b}[35;1m(\u{1b}[0m\u{1b}[37m'qty'\u{1b}[0m\u{1b}[35;1m)->\u{1b}[0m\u{1b}[39;1mnullable\u{1b}[0m\u{1b}[35;1m();\u{1b}[0m\r\n",
    "    \u{1b}[90m 14\u{1b}[0m\u{1b}[90m▕ \u{1b}[0m\u{1b}[35;1m        });\u{1b}[0m\r\n",
    "\u{1b}[31;1m  ➜ \u{1b}[0m\u{1b}[3;1m 15\u{1b}[0m\u{1b}[90m▕ \u{1b}[0m\u{1b}[35;1m        throw new \u{1b}[0m\u{1b}[39;1mRuntimeException\u{1b}[0m\u{1b}[35;1m(\u{1b}[0m\u{1b}[37m'boom from a migration'\u{1b}[0m\u{1b}[35;1m);\u{1b}[0m\r\n",
    "    \u{1b}[90m 16\u{1b}[0m\u{1b}[90m▕ \u{1b}[0m\u{1b}[35;1m    }\u{1b}[0m\r\n",
    "    \u{1b}[90m 17\u{1b}[0m\u{1b}[90m▕ \u{1b}[0m\u{1b}[35;1m};\u{1b}[0m\r\n",
    "    \u{1b}[90m 18\u{1b}[0m\u{1b}[90m▕ \u{1b}[0m\r\n",
    "\r\n",
    "      \u{1b}[2m+27 vendor frames \u{1b}[22m\r\n",
    "\r\n",
    "  \u{1b}[33m28  \u{1b}[39m\u{1b}[39;1martisan\u{1b}[39;22m:\u{1b}[39;1m16\u{1b}[39;22m\r\n",
    "  \u{1b}[90m    Illuminate\\Foundation\\Application::handleCommand(Object(Symfony\\Component\\Console\\Input\\ArgvInput))\u{1b}[39m\r\n",
    "\r\n",
);

const C1_THROW_L13_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_02_01_000001_throw_after_alter_orders_table .. \n",
    "\n",
    "   RuntimeException \n",
    "\n",
    "  boom from a migration\n",
    "\n",
    "  at database/migrations/step10/c1_throw/2026_02_01_000001_throw_after_alter_orders_table.php:15\n",
    "     11▕     {\n",
    "     12▕         Schema::table('orders', function (Blueprint $table) {\n",
    "     13▕             $table->integer('qty')->nullable();\n",
    "     14▕         });\n",
    "  ➜  15▕         throw new RuntimeException('boom from a migration');\n",
    "     16▕     }\n",
    "     17▕ };\n",
    "     18▕\n",
    "\n",
    "      \u{1b}[2m+27 vendor frames \u{1b}[22m\n",
    "\n",
    "  28  artisan:16\n",
    "      Illuminate\\Foundation\\Application::handleCommand(Object(Symfony\\Component\\Console\\Input\\ArgvInput))\n",
    "\n",
);

const C1_THROW_L12_SYMFONY: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000001_throw_after_alter_orders_table ...........................  \n",
    "\n",
    "In 2026_02_01_000001_throw_after_alter_orders_table.php line 15:\n",
    "                         \n",
    "  boom from a migration  \n",
    "                         \n",
    "\n",
);

const C3_L12: &str = concat!(
    "\n",
    "   ParseError \n",
    "\n",
    "  syntax error, unexpected token \"}\"\n",
    "\n",
    "  at database/migrations/step10/c3_syntax_error/2026_02_01_000003_syntax_error_in_orders_table.php:13\n",
    "      9▕     public function up(): void\n",
    "     10▕     {\n",
    "     11▕         Schema::table('orders', function (Blueprint $table) {\n",
    "     12▕             $table->integer('qty')->nullable()\n",
    "  ➜  13▕         });\n",
    "     14▕     }\n",
    "     15▕ };\n",
    "     16▕\n",
    "\n",
    "      \u{1b}[2m+20 vendor frames \u{1b}[22m\n",
    "\n",
    "  21  artisan:16\n",
    "      Illuminate\\Foundation\\Application::handleCommand(Object(Symfony\\Component\\Console\\Input\\ArgvInput))\n",
    "\n",
);

const C3_L12_SYMFONY: &str = concat!(
    "\n",
    "In 2026_02_01_000003_syntax_error_in_orders_table.php line 13:\n",
    "                                      \n",
    "  syntax error, unexpected token \"}\"  \n",
    "                                      \n",
    "\n",
);

const C4_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000041_first_alter_orders_table .................................  \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
    "  2026_02_01_000042_second_throws_orders_table ...............................  \n",
    "\n",
    "   RuntimeException \n",
    "\n",
    "  boom from a migration\n",
    "\n",
    "  at database/migrations/step10/c4_three_error_in_2nd/2026_02_01_000042_second_throws_orders_table.php:15\n",
    "     11▕     {\n",
    "     12▕         Schema::table('orders', function (Blueprint $table) {\n",
    "     13▕             $table->integer('qty2')->nullable();\n",
    "     14▕         });\n",
    "  ➜  15▕         throw new RuntimeException('boom from a migration');\n",
    "     16▕     }\n",
    "     17▕ };\n",
    "     18▕\n",
    "\n",
    "      \u{1b}[2m+27 vendor frames \u{1b}[22m\n",
    "\n",
    "  28  artisan:16\n",
    "      Illuminate\\Foundation\\Application::handleCommand(Object(Symfony\\Component\\Console\\Input\\ArgvInput))\n",
    "\n",
);

const C4_L12_SYMFONY: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000041_first_alter_orders_table .................................  \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
    "  2026_02_01_000042_second_throws_orders_table ...............................  \n",
    "\n",
    "In 2026_02_01_000042_second_throws_orders_table.php line 15:\n",
    "                         \n",
    "  boom from a migration  \n",
    "                         \n",
    "\n",
);

const C6_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000006_unguarded_read_orders_table ..............................  \n",
    "  ⇂ select count(*) as aggregate from `orders`  \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
    "\n",
);

const C8_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000008_other_reads_orders_table .................................  \n",
    "  ⇂ select id from orders where amount = 5  \n",
    "  ⇂ SELECT id FROM orders LIMIT 1  \n",
    "  ⇂ select count(*) from orders  \n",
    "  ⇂ select * from `orders` where `amount` = 5 limit 1  \n",
    "  ⇂ show index from orders  \n",
    "  ⇂ select index_name as `name`, group_concat(column_name order by seq_in_index) as `columns`, index_type as `type`, not non_unique as `unique` from information_schema.statistics where table_schema = schema() and table_name = 'orders' group by index_name, index_type, non_unique  \n",
    "  ⇂ select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as `collation`, is_nullable as `nullable`, column_default as `default`, column_comment as `comment`, generation_expression as `expression`, extra as `extra` from information_schema.columns where table_schema = schema() and table_name = 'orders' order by ordinal_position asc  \n",
    "  ⇂ select table_name as `name`, table_schema as `schema`, (data_length + index_length) as `size`, table_comment as `comment`, engine as `engine`, table_collation as `collation` from information_schema.tables where table_type in ('BASE TABLE', 'SYSTEM VERSIONED') and table_schema not in ('information_schema', 'mysql', 'ndbinfo', 'performance_schema', 'sys') order by table_schema, table_name  \n",
    "  ⇂ select 1  \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
    "\n",
);

const C9_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  AddQtyToOrdersTable ........................................................  \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
    "\n",
);

const C10_L13_SHORT: &str = concat!("\n", " INFO Nothing to migrate. \n", "\n",);

const THROW_NAME: &str = "2026_02_01_000001_throw_after_alter_orders_table";
const SYNTAX_ERROR_NAME: &str = "2026_02_01_000003_syntax_error_in_orders_table";
const FIRST_NAME: &str = "2026_02_01_000041_first_alter_orders_table";
const SECOND_NAME: &str = "2026_02_01_000042_second_throws_orders_table";
const READ_NOTE: &str = "this migration reads the database: under --pretend every read returns \
     nothing, so statements that depend on what it reads may be missing here, or may appear \
     although they would not run";
const NOTHING_CHECKED: &str =
    "nothing checked: Laravel stopped with an error before it printed a statement";

fn stopped_in(name: &str) -> String {
    format!(
        "Laravel stopped with an error in {name}; its statements and the migrations after it \
         were not seen"
    )
}

fn json_of(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn an_error_screen_in_the_first_migration_is_named_and_nothing_is_checked() {
    // Schema::table(...) then throw new RuntimeException(...): Laravel prints
    // the migration's name, then its error screen.
    for (form, input) in [
        ("Collision", C1_THROW_L12),
        ("Collision, terminal", C1_THROW_L12_PTY),
        ("Collision, short form", C1_THROW_L13_SHORT),
        ("Symfony", C1_THROW_L12_SYMFONY),
    ] {
        let output = kilit(&["check"], input);
        let text = stdout(&output);
        assert!(
            text.ends_with(&format!(
                "\n\n{}\n\n{NOTHING_CHECKED}\n",
                stopped_in(THROW_NAME)
            )),
            "{form}: {text}"
        );
        for name_or_screen in ["RuntimeException", "▕", "vendor frames", "handleCommand"] {
            assert!(!text.contains(name_or_screen), "{form}: {text}");
        }
        assert_eq!(output.status.code(), Some(2), "{form}: {text}");
    }
}

#[test]
fn an_error_screen_exits_with_2_also_with_strict() {
    for input in [C1_THROW_L12, C3_L12, C4_L12, C4_L12_SYMFONY] {
        for args in [&["check"][..], &["check", "--strict"][..]] {
            let output = kilit(args, input);
            assert_eq!(
                output.status.code(),
                Some(2),
                "{args:?}: {}",
                stdout(&output)
            );
        }
    }
}

#[test]
fn the_error_is_a_laravel_error_warning_in_the_json_and_the_exit_code_is_2() {
    for args in [
        &["check", "--format", "json"][..],
        &["check", "--format", "json", "--strict"][..],
    ] {
        let output = kilit(args, C1_THROW_L12);
        let json = json_of(&output);
        assert_eq!(json["sources"], serde_json::json!([]), "{args:?}");
        assert_eq!(json["unreadable"], serde_json::json!([]), "{args:?}");
        assert_eq!(
            json["warnings"],
            serde_json::json!([{
                "id": "laravel-error",
                "tables": [],
                "file": "standard input",
                "message": stopped_in(THROW_NAME),
            }]),
            "{args:?}"
        );
        assert_eq!(json["summary"]["statements"], 0, "{args:?}");
        assert_eq!(json["exit_code"], 2, "{args:?}");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn statements_printed_before_the_error_keep_their_answers_in_text_and_json() {
    // Three migrations: an ALTER (ADD COLUMN ... null, INSTANT), one that
    // throws, an ALTER that Laravel never reaches.
    for input in [C4_L12, C4_L12_SYMFONY] {
        let output = kilit(&["check"], input);
        let text = stdout(&output);
        let migration = text.find(FIRST_NAME).expect(&text);
        let answer = text
            .find("    INSTANT, no rebuild, writes continue\n")
            .expect(&text);
        let error = text.find(&stopped_in(SECOND_NAME)).expect(&text);
        assert!(migration < answer && answer < error, "{text}");
        assert!(!text.contains(&format!("\n{SECOND_NAME}\n")), "{text}");
        assert!(
            text.contains(&format!("\n\n{}\n\n", stopped_in(SECOND_NAME))),
            "{text}"
        );
        assert!(
            text.ends_with("\n1 statement: 1 runs while writes continue\n"),
            "{text}"
        );
        assert_eq!(output.status.code(), Some(2), "{text}");

        let json = json_of(&kilit(&["check", "--format", "json"], input));
        assert_eq!(json["sources"].as_array().unwrap().len(), 1);
        assert_eq!(json["sources"][0]["name"], FIRST_NAME);
        assert_eq!(
            json["sources"][0]["statements"][0]["impact"],
            "writes_continue"
        );
        assert_eq!(json["warnings"][0]["id"], "laravel-error");
        assert_eq!(json["warnings"][0]["message"], stopped_in(SECOND_NAME));
        assert_eq!(json["exit_code"], 2);
    }
}

#[test]
fn a_parse_error_names_the_migration_file_and_says_no_statement_was_seen() {
    // A missing semicolon: ParseError, and Laravel prints no migration.
    let message = format!(
        "Laravel stopped with an error in {SYNTAX_ERROR_NAME} before it printed any migration; \
         no statement was seen"
    );
    for (form, input) in [("Collision", C3_L12), ("Symfony", C3_L12_SYMFONY)] {
        let output = kilit(&["check"], input);
        let text = stdout(&output);
        assert!(
            text.ends_with(&format!("\n\n{message}\n\n{NOTHING_CHECKED}\n")),
            "{form}: {text}"
        );
        assert_eq!(output.status.code(), Some(2), "{form}: {text}");

        let json = json_of(&kilit(&["check", "--format", "json"], input));
        assert_eq!(json["warnings"][0]["id"], "laravel-error", "{form}");
        assert_eq!(json["warnings"][0]["message"], message, "{form}");
        assert_eq!(json["exit_code"], 2, "{form}");
    }
}

#[test]
fn the_error_warning_comes_before_the_metadata_lock_warning_and_the_exit_code_stays_2() {
    // The demo's migrations make writes wait on orders (exit code 1 alone);
    // after the last one, Laravel prints an error screen. Edited input: the
    // screen is Collision's, as printed for c1, with the file of the last
    // migration, so the migration has statements.
    let schema = example("mysql-schema.sql");
    let schema = schema.to_str().unwrap();
    let demo = std::fs::read_to_string(example("laravel-pretend.txt")).unwrap();
    let input = format!(
        "{demo}\n   RuntimeException \n\n  boom from a migration\n\n  at \
         database/migrations/2026_11_02_090500_add_note_search_to_orders_table.php:15\n\n"
    );
    let message = "Laravel stopped with an error after \
                   2026_11_02_090500_add_note_search_to_orders_table; the migrations after it \
                   were not seen";

    let without_error = kilit(&["check", "--schema", schema], &demo);
    assert_eq!(without_error.status.code(), Some(1));

    for strict in [false, true] {
        let mut args = vec!["check", "--schema", schema, "--format", "json"];
        if strict {
            args.push("--strict");
        }
        let output = kilit(&args, &input);
        let json = json_of(&output);
        assert_eq!(json["warnings"][0]["id"], "laravel-error");
        assert_eq!(json["warnings"][0]["tables"], serde_json::json!([]));
        assert_eq!(json["warnings"][0]["message"], message);
        assert_eq!(json["warnings"][1]["id"], "metadata-lock");
        assert_eq!(json["warnings"].as_array().unwrap().len(), 2);
        assert_eq!(json["sources"].as_array().unwrap().len(), 6);
        assert_eq!(json["exit_code"], 2);
        assert_eq!(output.status.code(), Some(2));
    }

    let text = stdout(&kilit(&["check", "--schema", schema], &input));
    let error = text.find(message).expect(&text);
    let lock = text.find("\nmetadata lock on orders:\n").expect(&text);
    assert!(error < lock, "{text}");
    assert!(text.contains(&format!("\n\n{message}\n\n")), "{text}");
}

#[test]
fn a_read_gets_one_note_under_its_first_statement_and_changes_no_answer_or_exit_code() {
    // DB::table('orders')->count() then Schema::table('orders', ...): the
    // select is printed, then the ALTER.
    let output = kilit(&["check"], C6_L12);
    let text = stdout(&output);
    assert_eq!(text.matches(READ_NOTE).count(), 1, "{text}");
    let select = text
        .find("select count(*) as aggregate from `orders`")
        .expect(&text);
    let note = text.find(&format!("    - {READ_NOTE}\n")).expect(&text);
    let alter = text
        .find("alter table `orders` add `qty` int null")
        .expect(&text);
    assert!(select < note && note < alter, "{text}");
    assert!(
        text.contains("    INSTANT, no rebuild, writes continue\n"),
        "{text}"
    );
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert_eq!(kilit(&["check", "--strict"], C6_L12).status.code(), Some(0));

    for args in [
        &["check", "--format", "json"][..],
        &["check", "--format", "json", "--strict"][..],
    ] {
        let output = kilit(args, C6_L12);
        let json = json_of(&output);
        let statements = &json["sources"][0]["statements"];
        assert_eq!(
            statements[0]["notes"],
            serde_json::json!([READ_NOTE]),
            "{args:?}"
        );
        assert!(
            !statements[1].to_string().contains("reads the database"),
            "{args:?}"
        );
        assert_eq!(statements[1]["impact"], "writes_continue", "{args:?}");
        // The metadata lock warning of every ALTER is the only warning.
        assert_eq!(json["warnings"].as_array().unwrap().len(), 1, "{args:?}");
        assert_eq!(json["warnings"][0]["id"], "metadata-lock", "{args:?}");
        assert_eq!(json["exit_code"], 0, "{args:?}");
        assert_eq!(output.status.code(), Some(0), "{args:?}");
    }
}

#[test]
fn ten_reads_in_one_migration_give_the_note_once() {
    // select, selectOne, scalar, first, `show index`, hasIndex,
    // getColumnListing, getTables, DB::statement('select 1'), then an ALTER.
    let text = stdout(&kilit(&["check"], C8_L12));
    assert_eq!(text.matches(READ_NOTE).count(), 1, "{text}");
    let note = text.find(READ_NOTE).unwrap();
    let second_read = text.find("SELECT id FROM orders LIMIT 1").expect(&text);
    assert!(note < second_read, "{text}");

    let json = json_of(&kilit(&["check", "--format", "json"], C8_L12));
    let statements = json["sources"][0]["statements"].as_array().unwrap();
    assert_eq!(statements.len(), 10);
    for (i, statement) in statements.iter().enumerate() {
        let noted = statement["notes"] == serde_json::json!([READ_NOTE]);
        assert_eq!(noted, i == 0, "statement {i}");
    }
    assert_eq!(json["exit_code"], 0);
}

#[test]
fn the_demo_has_no_read_note_and_no_laravel_error() {
    let output = laravel_demo(&[]);
    let text = stdout(&output);
    assert!(!text.contains("reads the database"), "{text}");
    assert!(!text.contains("Laravel stopped"), "{text}");
    let json = json_of(&laravel_demo(&["--format", "json"]));
    assert_eq!(json["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(json["warnings"][0]["id"], "metadata-lock");
    assert!(!json.to_string().contains("reads the database"));
}

#[test]
fn a_class_named_migration_and_nothing_to_migrate_are_read_as_before() {
    // class AddQtyToOrdersTable extends Migration: Laravel prints the class.
    let output = kilit(&["check"], C9_L12);
    let text = stdout(&output);
    assert!(
        text.contains(
            "\nAddQtyToOrdersTable\n  alter table `orders` add `qty` int null\n    INSTANT, no rebuild, writes continue\n"
        ),
        "{text}"
    );
    assert_eq!(output.status.code(), Some(0), "{text}");

    let output = kilit(&["check"], C10_L13_SHORT);
    let text = stdout(&output);
    assert!(
        text.ends_with("\nnothing to check: Laravel has no pending migration\n"),
        "{text}"
    );
    assert_eq!(output.status.code(), Some(0), "{text}");
}

#[test]
fn the_oem_code_page_forms_of_the_statement_mark_are_read_and_exit_with_0() {
    // The mark's bytes E2 87 82 as Windows code pages 720, 737 and 775 show
    // them.
    for mark in ["ظçé", "έΘΓ", "Ōćé"] {
        let input = format!(
            "\n   INFO  Running migrations.  \n\n  2026_02_01_000001_add_qty_to_orders_table ....  \n  {mark} alter table `orders` add `qty` int null  \n\n"
        );
        let output = kilit(&["check"], &input);
        let text = stdout(&output);
        assert!(!text.contains("could not read"), "{mark}: {text}");
        assert!(
            text.contains("alter table `orders` add `qty` int null\n    INSTANT, no rebuild, writes continue\n"),
            "{mark}: {text}"
        );
        assert_eq!(output.status.code(), Some(0), "{mark}: {text}");
        let json = json_of(&kilit(&["check", "--format", "json"], &input));
        assert_eq!(json["unreadable"], serde_json::json!([]), "{mark}");
        assert_eq!(json["exit_code"], 0, "{mark}");
    }
}

#[test]
fn sql_with_lines_that_look_like_an_error_screen_is_checked_as_sql() {
    let alter = "ALTER TABLE orders DROP COLUMN note;\n";
    let plain = kilit(&["check", "--format", "json"], alter);
    let plain_json = json_of(&plain);
    for comment in [
        "/*\n\nRollout\n\nat 10:30\n*/\n",
        "/*\n\nnotes\n\nat 20261106:1\n*/\n",
        "/*\n\nRollout\n\nIn plan line 3:\n\n*/\n",
    ] {
        let input = format!("{comment}{alter}");
        let output = kilit(&["check", "--format", "json"], &input);
        let json = json_of(&output);
        assert_eq!(json["sources"][0]["kind"], "sql", "{comment}");
        assert_eq!(json["unreadable"], serde_json::json!([]), "{comment}");
        assert!(
            !json["warnings"].to_string().contains("laravel-error"),
            "{comment}"
        );
        assert_eq!(
            json["sources"][0]["statements"][0]["impact"],
            plain_json["sources"][0]["statements"][0]["impact"],
            "{comment}"
        );
        assert_eq!(json["exit_code"], plain_json["exit_code"], "{comment}");
        assert_eq!(output.status.code(), plain.status.code(), "{comment}");
        let text = stdout(&kilit(&["check"], &input));
        assert!(!text.contains("Laravel stopped"), "{comment}: {text}");
    }
}

// ---------------------------------------------------------------------------
// Step 11a: what kilit prints. Rows that a new table cannot have, one line for
// a later read, and the wording of the foreign_key_checks and binary log notes.

const ASSUMES_CHECKS_ON: &str =
    "assumes foreign_key_checks is ON, the MySQL default: nothing before this statement sets it";
const ASSUMES_CHECKS_ON_IN_SESSION: &str = "assumes foreign_key_checks is ON in this session, the \
     MySQL default: a statement before this one changes its global value, which does not change \
     this session";
const ASSUMES_BINARY_LOG: &str = "assumes the binary log is on with binlog_format = ROW, the \
     MySQL default";
const ASSUMES_BINARY_LOG_BEFORE_11A: &str = "assumes the binary log is on, the MySQL default";
const CREATED_EARLIER: &str =
    "is created earlier in these migrations: nothing else uses it yet, so nothing waits for it";

/// Runs `args` with `input` as text and as JSON. Returns the text, the JSON
/// document and the exit status of the text run.
fn text_and_json(args: &[&str], input: &str) -> (String, serde_json::Value, Option<i32>) {
    let mut all = vec!["check"];
    all.extend(args);
    let output = kilit(&all, input);
    let text = stdout(&output);
    let code = output.status.code();
    all.extend(["--format", "json"]);
    let json = json_of(&kilit(&all, input));
    (text, json, code)
}

fn notes_of(statement: &serde_json::Value) -> Vec<String> {
    statement["notes"]
        .as_array()
        .map(|notes| {
            notes
                .iter()
                .map(|note| note.as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The notes of the last statement of the first source.
fn last_notes(json: &serde_json::Value) -> Vec<String> {
    notes_of(
        json["sources"][0]["statements"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
    )
}

/// Three tables created by the input. `children` is InnoDB, `legacy` MyISAM.
const NEW_TABLES: &str = "create table `parents` (`id` bigint unsigned not null auto_increment primary key);\n\
create table `children` (`id` bigint unsigned not null auto_increment primary key, `parent_id` bigint unsigned not null, `code` varchar(20) null, `qty` int not null, `maybe` int null) engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n\
create table `legacy` (`id` bigint unsigned not null auto_increment primary key, `code` varchar(20) null) engine=MyISAM default charset=utf8mb4;\n";

/// An ALTER whose answer carries a note about rows already in the table.
struct RowNote {
    label: &'static str,
    table: &'static str,
    alter: &'static str,
    prefix: &'static str,
}

const ROW_NOTES: [RowNote; 8] = [
    RowNote {
        label: "1264",
        table: "children",
        alter: "alter table `children` modify `qty` smallint not null;",
        prefix: "if a value does not fit the new type (in strict SQL mode, the default): ",
    },
    RowNote {
        label: "longer than the new length",
        table: "children",
        alter: "alter table `children` modify `code` varchar(10) null;",
        prefix: "if a value is longer than the new length (in strict SQL mode, the default): ",
    },
    RowNote {
        label: "NULL",
        table: "children",
        alter: "alter table `children` modify `maybe` int not null;",
        prefix: "if a row holds NULL: ",
    },
    RowNote {
        label: "duplicates, InnoDB",
        table: "children",
        alter: "alter table `children` add unique `children_code_unique`(`code`);",
        prefix: "if the columns hold duplicate values: ",
    },
    RowNote {
        label: "duplicates, MyISAM",
        table: "legacy",
        alter: "alter table `legacy` add unique `legacy_code_unique`(`code`);",
        prefix: "if the columns hold duplicate values: ",
    },
    RowNote {
        label: "1452",
        table: "children",
        alter: "alter table `children` add constraint `children_parent_id_foreign` foreign key \
                (`parent_id`) references `parents` (`id`);",
        prefix: "if a row points to a parent row that does not exist: ",
    },
    RowNote {
        label: "foreign key with the checks off",
        table: "children",
        alter: "set foreign_key_checks = 0;\n\
                alter table `children` add constraint `children_parent_id_foreign` foreign key \
                (`parent_id`) references `parents` (`id`);",
        prefix: "if a row points to a parent row that does not exist (the row is kept unchecked): ",
    },
    RowNote {
        label: "check constraint",
        table: "children",
        alter: "alter table `children` add constraint `children_qty_check` check (qty > 0);",
        prefix: "if a row breaks the constraint: ",
    },
];

#[test]
fn row_notes_are_left_out_of_the_text_for_a_table_the_input_created_but_stay_in_the_json() {
    for case in &ROW_NOTES {
        let input = format!("{NEW_TABLES}{}\n", case.alter);
        let (text, json, code) = text_and_json(&[], &input);
        assert!(!text.contains(case.prefix), "{}: {text}", case.label);
        assert!(
            text.contains(&format!("    - {} {CREATED_EARLIER}\n", case.table)),
            "{}: {text}",
            case.label
        );
        let notes = last_notes(&json);
        assert!(
            notes.iter().any(|note| note.starts_with(case.prefix)),
            "{}: {notes:?}",
            case.label
        );
        // Neither the answer nor the exit code moves.
        assert_eq!(
            json["sources"][0]["statements"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["impact"],
            "new_table",
            "{}",
            case.label
        );
        assert_eq!(code, Some(0), "{}: {text}", case.label);
        assert_eq!(json["exit_code"], 0, "{}", case.label);
    }
}

#[test]
fn only_the_row_line_goes_from_a_new_tables_text_and_the_json_notes_are_what_they_were() {
    // $table->smallInteger('qty')->change() on a table created in the same
    // input: the text loses the 1264 line and nothing else.
    let input = format!("{NEW_TABLES}alter table `children` modify `qty` smallint not null;\n");
    let (text, json, _) = text_and_json(&[], &input);
    assert!(
        text.contains(
            "  line 4: alter table `children` modify `qty` smallint not null\n\
             \x20   COPY, copies every row, writes wait\n\
             \x20   measured as change-integer-type (3 lab scenarios)\n\
             \x20   - changes the type from INT to SMALLINT\n\
             \x20   - children is created earlier in these migrations: nothing else uses it yet, \
             so nothing waits for it\n"
        ),
        "{text}"
    );
    assert!(
        text.ends_with("\n4 statements: 1 changes a table created here, 3 not checked\n"),
        "{text}"
    );
    assert_eq!(
        last_notes(&json),
        [
            "changes the type from INT to SMALLINT",
            "if a value does not fit the new type (in strict SQL mode, the default): fails with \
             error 1264 (int-to-tinyint-out-of-range)",
        ]
    );
    let sql = "alter table `children` add constraint `children_parent_id_foreign` foreign key \
               (`parent_id`) references `parents` (`id`);";
    let (_, json, _) = text_and_json(&[], &format!("{NEW_TABLES}{sql}\n"));
    assert_eq!(
        last_notes(&json),
        [
            ASSUMES_CHECKS_ON,
            "if a row points to a parent row that does not exist: fails with error 1452 \
             (add-foreign-key-orphans)",
        ]
    );
    assert_eq!(json["summary"]["new_table"], 1);
}

/// Statements that may put rows into table `t` (or that kilit cannot rule
/// out), all after the CREATE and before the ALTER.
fn statements_that_may_put_rows_into(t: &str) -> Vec<String> {
    vec![
        format!("insert into `{t}` (`id`) values (1)"),
        format!("INSERT INTO {} (id) VALUES (1)", t.to_uppercase()),
        format!("insert into {t} set id = 1"),
        format!("replace into `{t}` (`id`) values (1)"),
        format!("update `{t}` set `id` = `id` + 1"),
        format!("insert into `shop`.`{t}` (`id`) values (1)"),
        format!("load data infile '/tmp/{t}.csv' into table `{t}`"),
        // Not a statement kilit reads.
        format!("alter table `staging` exchange partition p0 with table `{t}`"),
        format!("select * from `{t}`"),
        format!("delete from {t}"),
        // A name that contains the table's name counts: the scan is a text
        // scan, on the safe side.
        format!("insert into `{t}_archive` (`id`) values (1)"),
    ]
}

#[test]
fn a_statement_that_may_have_put_rows_into_the_table_keeps_its_row_notes() {
    for case in &ROW_NOTES {
        for between in statements_that_may_put_rows_into(case.table) {
            let input = format!("{NEW_TABLES}{between};\n{}\n", case.alter);
            let (text, json, _) = text_and_json(&[], &input);
            // A statement kilit cannot read makes it lose the table, and a
            // MODIFY (or a MyISAM change) on a lost table has no answer, so no row note.
            if text.contains("kilit lost track of") {
                continue;
            }
            assert!(
                text.contains(&format!("    - {}", case.prefix)),
                "{} after `{between}`: {text}",
                case.label
            );
            assert!(
                last_notes(&json)
                    .iter()
                    .any(|note| note.starts_with(case.prefix)),
                "{} after `{between}`",
                case.label
            );
        }
    }
}

#[test]
fn a_call_or_an_execute_keeps_the_row_notes_of_every_new_table() {
    // A stored procedure or a prepared statement can fill any table, and
    // its text does not name the table.
    for between in [
        "call fill_all_tables()",
        "CALL fill_all_tables()",
        "execute stmt1",
        "EXECUTE stmt1 USING @a",
    ] {
        for case in &ROW_NOTES {
            let input = format!("{NEW_TABLES}{between};\n{}\n", case.alter);
            let (text, _, _) = text_and_json(&[], &input);
            assert!(
                text.contains(&format!("    - {}", case.prefix)),
                "{} after `{between}`: {text}",
                case.label
            );
        }
    }
}

#[test]
fn statements_that_cannot_have_put_rows_into_the_table_leave_the_row_notes_out() {
    for case in &ROW_NOTES {
        let t = case.table;
        for between in [
            "insert into `elsewhere` (`id`) values (1)".to_owned(),
            "select 1".to_owned(),
            "set @x = 1".to_owned(),
            "create table `elsewhere` (`id` int not null)".to_owned(),
            "alter table `parents` add `extra` int null".to_owned(),
            // An ALTER that kilit reads fully puts no rows in the table.
            format!("alter table `{t}` add `extra_col` int null"),
        ] {
            let input = format!("{NEW_TABLES}{between};\n{}\n", case.alter);
            let (text, json, _) = text_and_json(&[], &input);
            assert!(
                !text.contains(case.prefix),
                "{} after `{between}`: {text}",
                case.label
            );
            assert!(
                last_notes(&json)
                    .iter()
                    .any(|note| note.starts_with(case.prefix)),
                "{} after `{between}`",
                case.label
            );
        }
        // A statement after the ALTER cannot have put rows in before it.
        let input = format!(
            "{NEW_TABLES}{}\ninsert into `{t}` (`id`) values (1);\n",
            case.alter
        );
        let (text, _, _) = text_and_json(&[], &input);
        assert!(
            !text.contains(case.prefix),
            "{} before an insert: {text}",
            case.label
        );
    }
}

#[test]
fn a_table_created_again_after_its_rows_were_dropped_has_no_row_notes() {
    let input = "create table `children` (`id` int not null, `qty` int not null);
                 insert into children (id) values (1);
                 drop table `children`;
                 create table `children` (`id` int not null, `qty` int not null);
                 alter table `children` modify `qty` smallint not null;";
    let (text, json, _) = text_and_json(&[], input);
    assert!(
        !text.contains("if a value does not fit the new type"),
        "{text}"
    );
    assert!(
        last_notes(&json)
            .iter()
            .any(|note| note.starts_with("if a value does not fit the new type"))
    );
}

#[test]
fn tables_from_the_schema_file_keep_every_row_note() {
    // One table the input creates, two the schema file has: the row notes of
    // the schema's tables stay, the new table's goes.
    let schema = example("mysql-schema.sql");
    let input = format!(
        "{NEW_TABLES}alter table `children` modify `qty` smallint not null;\n\
         alter table `orders` modify `amount` smallint not null;\n\
         alter table `orders` add unique `orders_note_unique`(`note`);\n"
    );
    let (text, json, _) = text_and_json(&["--schema", schema.to_str().unwrap()], &input);
    assert_eq!(
        text.matches("if a value does not fit the new type (in strict SQL mode, the default): ")
            .count(),
        1,
        "{text}"
    );
    assert_eq!(
        text.matches("    - if the columns hold duplicate values: ")
            .count(),
        1,
        "{text}"
    );
    let statements = json["sources"][0]["statements"].as_array().unwrap();
    for (index, prefix) in [
        (3, "if a value does not fit the new type"),
        (4, "if a value does not fit the new type"),
        (5, "if the columns hold duplicate values: "),
    ] {
        assert!(
            notes_of(&statements[index])
                .iter()
                .any(|note| note.starts_with(prefix)),
            "statement {index}"
        );
    }
}

#[test]
fn create_table_if_not_exists_keeps_the_row_notes() {
    // The table may have been there before: it is not "created here".
    let input = "create table if not exists `children` (`id` int not null, `qty` int not null);
                 alter table `children` modify `qty` smallint not null;";
    let (text, json, _) = text_and_json(&[], input);
    assert!(
        text.contains(
            "    - if a value does not fit the new type (in strict SQL mode, the default): "
        ),
        "{text}"
    );
    assert!(!text.contains(CREATED_EARLIER), "{text}");
    assert!(
        last_notes(&json)
            .iter()
            .any(|note| note.starts_with("if a value does not fit the new type"))
    );
}

#[test]
fn a_created_and_dropped_name_taken_by_an_old_table_keeps_the_row_notes() {
    // `shops` is in the schema file: it is renamed to a name that was created
    // and dropped before, and still holds its rows.
    let schema = example("mysql-schema.sql");
    let input = "create table `shops_tmp` (`id` int not null);
                 drop table `shops_tmp`;
                 rename table `shops` to `shops_tmp`;
                 alter table `shops_tmp` add unique `shops_tmp_name_unique`(`name`);";
    let (text, json, _) = text_and_json(&["--schema", schema.to_str().unwrap()], input);
    assert!(
        text.contains("    - if the columns hold duplicate values: "),
        "{text}"
    );
    assert!(
        last_notes(&json)
            .iter()
            .any(|note| note.starts_with("if the columns hold duplicate values: "))
    );
}

#[test]
fn notes_that_do_not_depend_on_rows_stay_for_a_new_table() {
    let input = format!(
        "{NEW_TABLES}alter table `children` add `extra` int null;\n\
         alter table `children` add constraint `children_parent_id_foreign` foreign key \
         (`parent_id`) references `parents` (`id`);\n"
    );
    let (text, json, _) = text_and_json(&[], &input);
    // The history of the table is not about rows.
    assert!(
        text.contains(
            "    - if the table has had 64 instant column changes since it was last rebuilt: \
             INPLACE, rebuilds the table, writes continue (add-column-after-64-changes)\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(&format!("    - {ASSUMES_CHECKS_ON}\n")),
        "{text}"
    );
    assert_eq!(text.matches(CREATED_EARLIER).count(), 2, "{text}");
    assert!(!text.contains("if a row points to a parent row"), "{text}");
    let statements = json["sources"][0]["statements"].as_array().unwrap();
    assert!(
        notes_of(&statements[3])
            .iter()
            .any(|note| note.starts_with("if the table has had 64 instant column changes"))
    );
}

// Laravel's shapes. $table->foreignId('parent_id') in a create, then an
// ALTER that adds the key; with and without an insert between them.

const L12_CREATE_FK: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_03_02_000002_create_parents_children_fk ...............................  \n",
    "  \u{21c2} create table `parents` (`id` bigint unsigned not null auto_increment primary key, `name` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  \u{21c2} create table `children` (`id` bigint unsigned not null auto_increment primary key, `parent_id` bigint unsigned not null, `name` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  \u{21c2} alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`)  \n",
    "\n",
);

const L13_SHORT_CREATE_FK: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_03_02_000002_create_parents_children_fk .. \n",
    " \u{21c2} create table `parents` (`id` bigint unsigned not null auto_increment primary key, `name` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " \u{21c2} create table `children` (`id` bigint unsigned not null auto_increment primary key, `parent_id` bigint unsigned not null, `name` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " \u{21c2} alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`) \n",
    "\n",
);

const L12_CREATE_INSERT_FK: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_03_02_000001_create_parents_children_insert_fk ........................  \n",
    "  \u{21c2} create table `parents` (`id` bigint unsigned not null auto_increment primary key, `name` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  \u{21c2} create table `children` (`id` bigint unsigned not null auto_increment primary key, `parent_id` bigint unsigned not null, `name` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  \u{21c2} insert into `children` (`name`, `parent_id`) values ('a', 1)  \n",
    "  \u{21c2} alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`)  \n",
    "\n",
);

const L13_SHORT_CREATE_INSERT_FK: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_03_02_000001_create_parents_children_insert_fk .. \n",
    " \u{21c2} create table `parents` (`id` bigint unsigned not null auto_increment primary key, `name` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " \u{21c2} create table `children` (`id` bigint unsigned not null auto_increment primary key, `parent_id` bigint unsigned not null, `name` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " \u{21c2} insert into `children` (`name`, `parent_id`) values ('a', 1) \n",
    " \u{21c2} alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`) \n",
    "\n",
);

const EXPECTED_CREATE_FK: &str = r#"checked against mysql-8.4.13, as measured by kilit-lab
no --schema given: kilit knows no table, so it cannot judge MODIFY and CHANGE

2026_03_02_000002_create_parents_children_fk
  create table `parents` (`id` bigint unsigned not null auto_increment primary key, `name` varchar(...
    not checked: creates a new table; no existing rows are touched
  create table `children` (`id` bigint unsigned not null auto_increment primary key, `parent_id` bi...
    not checked: creates a new table; no existing rows are touched
  alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`)
    COPY, copies every row, writes wait
    measured as add-foreign-key (16 lab scenarios)
    - assumes foreign_key_checks is ON, the MySQL default: nothing before this statement sets it
    - children is created earlier in these migrations: nothing else uses it yet, so nothing waits for it

3 statements: 1 changes a table created here, 2 not checked
"#;
const EXPECTED_CREATE_INSERT_FK: &str = r#"checked against mysql-8.4.13, as measured by kilit-lab
no --schema given: kilit knows no table, so it cannot judge MODIFY and CHANGE

2026_03_02_000001_create_parents_children_insert_fk
  create table `parents` (`id` bigint unsigned not null auto_increment primary key, `name` varchar(...
    not checked: creates a new table; no existing rows are touched
  create table `children` (`id` bigint unsigned not null auto_increment primary key, `parent_id` bi...
    not checked: creates a new table; no existing rows are touched
  insert into `children` (`name`, `parent_id`) values ('a', 1)
    not checked: not a schema change
  alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`)
    COPY, copies every row, writes wait
    measured as add-foreign-key (16 lab scenarios)
    - assumes foreign_key_checks is ON, the MySQL default: nothing before this statement sets it
    - if a row points to a parent row that does not exist: fails with error 1452 (add-foreign-key-orphans)
    - children is created earlier in these migrations: nothing else uses it yet, so nothing waits for it

4 statements: 1 changes a table created here, 3 not checked
"#;

#[test]
fn a_laravel_migration_that_creates_a_table_and_adds_a_key_loses_the_row_line_only() {
    // Schema::create('parents'), Schema::create('children') with
    // foreignId('parent_id'), then $table->foreign('parent_id')->references('id')
    // ->on('parents') in Schema::table. Laravel 12 and 13 in a terminal print
    // the long form, and Laravel 13 prints a shorter form in some environments.
    for (form, input) in [("long", L12_CREATE_FK), ("short", L13_SHORT_CREATE_FK)] {
        let output = kilit(&["check"], input);
        assert_eq!(stdout(&output), EXPECTED_CREATE_FK, "{form}");
        assert_eq!(output.status.code(), Some(0), "{form}");
        let json = json_of(&kilit(&["check", "--format", "json"], input));
        let statements = json["sources"][0]["statements"].as_array().unwrap();
        assert_eq!(
            notes_of(&statements[2]),
            [
                ASSUMES_CHECKS_ON,
                "if a row points to a parent row that does not exist: fails with error 1452 \
                 (add-foreign-key-orphans)"
            ],
            "{form}"
        );
        assert_eq!(json["exit_code"], 0, "{form}");
    }
}

#[test]
fn an_insert_between_the_create_and_the_key_keeps_the_row_line_in_laravel_s_output() {
    for (form, input) in [
        ("long", L12_CREATE_INSERT_FK),
        ("short", L13_SHORT_CREATE_INSERT_FK),
    ] {
        let output = kilit(&["check"], input);
        assert_eq!(stdout(&output), EXPECTED_CREATE_INSERT_FK, "{form}");
        assert_eq!(output.status.code(), Some(0), "{form}");
    }
}

// Reads. Schema::hasTable('users'), Schema::getColumnListing('users') twice,
// DB::table('users')->exists(), Schema::table(... add nickname), then
// Schema::getColumnListing('users') again; a second migration reads twice
// and adds a column.

const SELECT_HAS_TABLE: &str = "select exists (select 1 from information_schema.tables where table_schema = schema() and table_name = 'users' and table_type in ('BASE TABLE', 'SYSTEM VERSIONED')) as `exists`";
const SELECT_COLUMNS: &str = "select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as `collation`, is_nullable as `nullable`, column_default as `default`, column_comment as `comment`, generation_expression as `expression`, extra as `extra` from information_schema.columns where table_schema = schema() and table_name = 'users' order by ordinal_position asc";
const SELECT_EXISTS_USERS: &str = "select exists(select * from `users`) as `exists`";
const ADD_NICKNAME: &str = "alter table `users` add `nickname` varchar(255) null";
const ADD_PHONE: &str = "alter table `users` add `phone` varchar(255) null";

/// Laravel's long form (Laravel 12, and 13 in a terminal).
fn pretend_long(migrations: &[(&str, &[&str])]) -> String {
    let mut out = String::from("\n   INFO  Running migrations.  \n\n");
    for (name, statements) in migrations {
        out.push_str(&format!("  {name} ....  \n"));
        for sql in *statements {
            out.push_str(&format!("  \u{21c2} {sql}  \n"));
        }
    }
    out.push('\n');
    out
}

/// The shorter form Laravel 13 prints in some environments.
fn pretend_short(migrations: &[(&str, &[&str])]) -> String {
    let mut out = String::from("\n INFO Running migrations. \n\n");
    for (name, statements) in migrations {
        out.push_str(&format!(" {name} .. \n"));
        for sql in *statements {
            out.push_str(&format!(" \u{21c2} {sql} \n"));
        }
    }
    out.push('\n');
    out
}

fn reads_migrations() -> [(&'static str, &'static [&'static str]); 2] {
    [
        (
            "2026_03_01_000001_reads_one_users_table",
            &[
                SELECT_HAS_TABLE,
                SELECT_COLUMNS,
                SELECT_COLUMNS,
                SELECT_EXISTS_USERS,
                ADD_NICKNAME,
                SELECT_COLUMNS,
            ],
        ),
        (
            "2026_03_01_000002_reads_two_users_table",
            &[SELECT_HAS_TABLE, SELECT_COLUMNS, ADD_PHONE],
        ),
    ]
}

const EXPECTED_READS: &str = r#"checked against mysql-8.4.13, as measured by kilit-lab
no --schema given: kilit knows no table, so it cannot judge MODIFY and CHANGE

2026_03_01_000001_reads_one_users_table
  select exists (select 1 from information_schema.tables where table_schema = schema() and table_na...
    not checked: not a schema change
    - this migration reads the database: under --pretend every read returns nothing, so statements that depend on what it reads may be missing here, or may appear although they would not run
  read (not checked): select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as ...
  read (not checked): select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as ...
  read (not checked): select exists(select * from `users`) as `exists`
  alter table `users` add `nickname` varchar(255) null
    INSTANT, no rebuild, writes continue
    measured as add-column (7 lab scenarios)
    - if the table has a FULLTEXT index: COPY, copies every row, writes wait (add-column-with-fulltext)
    - if the table has had 64 instant column changes since it was last rebuilt: INPLACE, rebuilds the table, writes continue (add-column-after-64-changes)
  read (not checked): select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as ...

2026_03_01_000002_reads_two_users_table
  select exists (select 1 from information_schema.tables where table_schema = schema() and table_na...
    not checked: not a schema change
    - this migration reads the database: under --pretend every read returns nothing, so statements that depend on what it reads may be missing here, or may appear although they would not run
  read (not checked): select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as ...
  alter table `users` add `phone` varchar(255) null
    INSTANT, no rebuild, writes continue
    measured as add-column (7 lab scenarios)
    - if the table has a FULLTEXT index: COPY, copies every row, writes wait (add-column-with-fulltext)
    - if the table has had 64 instant column changes since it was last rebuilt: INPLACE, rebuilds the table, writes continue (add-column-after-64-changes)

metadata lock on users:
  Every ALTER TABLE, DROP TABLE, RENAME TABLE and TRUNCATE TABLE first waits
  for the open transactions that have used any of its tables to end. While it
  waits, new queries on those tables queue behind it, reads too, even for an
  INSTANT change, and by default MySQL lets it wait a year (lock_wait_timeout
  = 31536000). Run SET SESSION lock_wait_timeout = 5 before the migration to
  cap the wait: the statement then fails with error 1205 after 5 seconds, the
  queries behind it go on, and you can run the migration again later.
  measured by kilit-lab on mysql-8.4.13

9 statements: 2 run while writes continue, 7 not checked
"#;

#[test]
fn every_read_after_the_first_in_a_migration_is_one_line_without_an_explanation() {
    for (form, input) in [
        ("long", pretend_long(&reads_migrations())),
        ("short", pretend_short(&reads_migrations())),
    ] {
        let output = kilit(&["check"], &input);
        assert_eq!(stdout(&output), EXPECTED_READS, "{form}");
        // Reads change no exit code.
        assert_eq!(output.status.code(), Some(0), "{form}");
        assert_eq!(kilit(&["check", "--strict"], &input).status.code(), Some(0));
    }
}

#[test]
fn the_json_keeps_every_read_and_the_read_note_as_before() {
    for (form, input) in [
        ("long", pretend_long(&reads_migrations())),
        ("short", pretend_short(&reads_migrations())),
    ] {
        let json = json_of(&kilit(&["check", "--format", "json"], &input));
        let sources = json["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2, "{form}");
        let counts: Vec<usize> = sources
            .iter()
            .map(|source| source["statements"].as_array().unwrap().len())
            .collect();
        assert_eq!(counts, [6, 3], "{form}");
        // The whole SQL of every statement, uncut.
        let sql: Vec<&str> = sources[0]["statements"]
            .as_array()
            .unwrap()
            .iter()
            .map(|statement| statement["sql"].as_str().unwrap())
            .collect();
        assert_eq!(
            sql,
            [
                SELECT_HAS_TABLE,
                SELECT_COLUMNS,
                SELECT_COLUMNS,
                SELECT_EXISTS_USERS,
                ADD_NICKNAME,
                SELECT_COLUMNS
            ],
            "{form}"
        );
        for (source, reads) in [(0, [0, 1, 2, 3, 5].as_slice()), (1, [0, 1].as_slice())] {
            let statements = sources[source]["statements"].as_array().unwrap();
            for &read in reads {
                assert_eq!(statements[read]["impact"], "not_checked", "{form}");
                assert_eq!(
                    statements[read]["reason"], "not a schema change",
                    "{form}: source {source} statement {read}"
                );
                // Only the first read of a migration has the note.
                let noted = statements[read]["notes"] == serde_json::json!([READ_NOTE]);
                assert_eq!(noted, read == 0, "{form}: source {source} statement {read}");
            }
        }
        assert_eq!(sources[0]["statements"][4]["impact"], "writes_continue");
        assert_eq!(sources[1]["statements"][2]["impact"], "writes_continue");
        assert_eq!(json["summary"]["statements"], 9, "{form}");
        assert_eq!(json["summary"]["not_checked"], 7, "{form}");
        assert_eq!(json["summary"]["writes_continue"], 2, "{form}");
        assert_eq!(json["exit_code"], 0, "{form}");
    }
}

fn one_migration(statements: &[&str]) -> String {
    pretend_long(&[("2026_03_03_000001_reads_orders_table", statements)])
}

#[test]
fn a_later_read_that_may_write_or_is_not_plainly_a_read_prints_in_full() {
    // with ... is not in the list of read words. The others hold a word that
    // writes: into, update, insert, delete, replace.
    for not_a_short_line in [
        "with x as (select 1) select * from x",
        "select id into @x from `orders` limit 1",
        "SELECT id INTO @x FROM `orders` LIMIT 1",
        "select * from `orders` into outfile '/tmp/orders.csv'",
        "select * from `orders` where id = 1 for update",
        "insert into `orders` (`id`) values (1)",
        "update `orders` set `qty` = 1",
        "delete from `orders`",
        "replace into `orders` (`id`) values (1)",
    ] {
        let input = one_migration(&["select 1", not_a_short_line, "select 2"]);
        let text = stdout(&kilit(&["check"], &input));
        // kilit cannot read INTO OUTFILE: its full form is the SQL line and
        // an UNKNOWN line, not "not checked".
        let full = if not_a_short_line.contains("outfile") {
            format!("\n  {not_a_short_line}\n    UNKNOWN: kilit could not read this statement")
        } else {
            format!("\n  {not_a_short_line}\n    not checked: not a schema change\n")
        };
        assert!(text.contains(&full), "{not_a_short_line}: {text}");
        assert!(
            !text.contains(&format!("read (not checked): {not_a_short_line}")),
            "{not_a_short_line}: {text}"
        );
        // The statement around it that is a plain read is a short line.
        assert!(
            text.contains("\n  read (not checked): select 2\n"),
            "{text}"
        );
        // The first read keeps its note, once.
        assert_eq!(
            text.matches(READ_NOTE).count(),
            1,
            "{not_a_short_line}: {text}"
        );
    }
}

#[test]
fn every_plain_read_word_gives_a_short_line_for_a_later_read_in_any_letter_case() {
    for read in [
        "select 1",
        "SELECT 1",
        "Select id from `orders`",
        "show tables",
        "SHOW INDEX FROM `orders`",
        "describe `orders`",
        "desc `orders`",
        "explain select * from `orders`",
        "values row(1, 2)",
        // A column that only starts with a write word is no write word.
        "select `updated_at` from `orders`",
    ] {
        let input = one_migration(&["select 0", read]);
        let text = stdout(&kilit(&["check"], &input));
        assert!(
            text.contains(&format!("\n  read (not checked): {read}\n")),
            "{read}: {text}"
        );
        assert!(!text.contains(&format!("\n  {read}\n")), "{read}: {text}");
        assert_eq!(text.matches(READ_NOTE).count(), 1, "{read}: {text}");
    }
}

#[test]
fn the_first_read_is_not_a_short_line_even_after_other_statements() {
    let input = one_migration(&[
        "alter table `orders` add `qty` int null",
        "select 1",
        "select 2",
    ]);
    let text = stdout(&kilit(&["check"], &input));
    assert!(
        text.contains(&format!(
            "  select 1\n    not checked: not a schema change\n    - {READ_NOTE}\n\
             \x20 read (not checked): select 2\n"
        )),
        "{text}"
    );
}

#[test]
fn a_read_over_100_characters_is_cut_to_97_and_three_dots_and_100_is_not_cut() {
    let exactly_100 = format!("select {} from t", "x".repeat(100 - "select  from t".len()));
    assert_eq!(exactly_100.len(), 100);
    let over = format!("{exactly_100} ");
    let over = format!("{over}x");
    assert_eq!(over.len(), 102);
    let input = one_migration(&["select 1", &exactly_100, &over]);
    let text = stdout(&kilit(&["check"], &input));
    assert!(
        text.contains(&format!("\n  read (not checked): {exactly_100}\n")),
        "{text}"
    );
    assert!(
        text.contains(&format!("\n  read (not checked): {}...\n", &over[..97])),
        "{text}"
    );
}

#[test]
fn a_plain_sql_file_keeps_every_select_with_its_line_and_reason() {
    let sql = "select 1;\nselect 2;\nshow tables;\nselect 3;";
    let (text, json, code) = text_and_json(&[], sql);
    for (line, statement) in [
        (1, "select 1"),
        (2, "select 2"),
        (3, "show tables"),
        (4, "select 3"),
    ] {
        assert!(
            text.contains(&format!(
                "  line {line}: {statement}\n    not checked: not a schema change\n"
            )),
            "{text}"
        );
    }
    assert!(!text.contains("read (not checked)"), "{text}");
    assert!(!text.contains("reads the database"), "{text}");
    assert_eq!(code, Some(0));
    assert_eq!(
        json["sources"][0]["statements"].as_array().unwrap().len(),
        4
    );
}

#[test]
fn a_global_foreign_key_checks_set_before_a_key_changes_the_assumption_note() {
    // SET GLOBAL ... changes the value of the next connection: this session
    // keeps the MySQL default, which is ON.
    let add_key = "alter table `children` add constraint `children_parent_id_foreign` foreign key \
                   (`parent_id`) references `parents` (`id`);";
    for set in [
        "SET GLOBAL foreign_key_checks = 0",
        "set global foreign_key_checks = off",
        "SET @@global.foreign_key_checks = 0",
        "SET PERSIST foreign_key_checks = 0",
        "SET @@persist.foreign_key_checks = 0",
    ] {
        let input = format!("{set};\n{NEW_TABLES}{add_key}\n");
        let (text, json, code) = text_and_json(&[], &input);
        assert!(
            text.contains(&format!("    - {ASSUMES_CHECKS_ON_IN_SESSION}\n")),
            "{set}: {text}"
        );
        assert!(!text.contains(ASSUMES_CHECKS_ON), "{set}: {text}");
        let statement = json["sources"][0]["statements"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(
            notes_of(&statement)[0],
            ASSUMES_CHECKS_ON_IN_SESSION,
            "{set}"
        );
        // The answer and the exit code are those of a key with the checks on.
        assert_eq!(statement["impact"], "new_table", "{set}");
        assert_eq!(statement["case"], "add-foreign-key", "{set}");
        assert_eq!(statement["algorithm"], "COPY", "{set}");
        assert_eq!(code, Some(0), "{set}");
    }
}

#[test]
fn the_assumption_note_keeps_its_text_when_nothing_before_the_statement_sets_the_checks() {
    let input = format!(
        "{NEW_TABLES}alter table `children` add constraint `children_parent_id_foreign` foreign \
         key (`parent_id`) references `parents` (`id`);\n"
    );
    let (text, json, _) = text_and_json(&[], &input);
    assert!(
        text.contains(&format!("    - {ASSUMES_CHECKS_ON}\n")),
        "{text}"
    );
    assert!(!text.contains("in this session"), "{text}");
    assert_eq!(last_notes(&json)[0], ASSUMES_CHECKS_ON);
}

#[test]
fn a_session_set_after_a_global_one_means_no_assumption_note_at_all() {
    let input = format!(
        "SET GLOBAL foreign_key_checks = 0;\nSET SESSION foreign_key_checks = 1;\n{NEW_TABLES}\
         alter table `children` add constraint `children_parent_id_foreign` foreign key \
         (`parent_id`) references `parents` (`id`);\n"
    );
    let (text, json, _) = text_and_json(&[], &input);
    assert!(!text.contains("assumes foreign_key_checks"), "{text}");
    assert!(
        !last_notes(&json)
            .iter()
            .any(|note| note.contains("assumes foreign_key_checks"))
    );
}

#[test]
fn dropping_a_foreign_key_column_or_its_only_index_says_the_session_assumption_in_the_new_words() {
    let schema = example("mysql-schema.sql");
    let schema = schema.to_str().unwrap();
    let sql = "alter table orders drop index orders_user_id_foreign;
               alter table orders drop column user_id;";
    // Nothing sets the checks: the words stay.
    let (text, json, code) = text_and_json(&["--schema", schema], sql);
    for note in [
        format!(
            "orders_user_id_foreign is the only index that serves the foreign key \
             orders_user_id_foreign; {ASSUMES_CHECKS_ON}"
        ),
        format!("user_id is in the foreign key orders_user_id_foreign; {ASSUMES_CHECKS_ON}"),
    ] {
        assert!(text.contains(&format!("    - {note}\n")), "{text}");
    }
    let before = json["sources"][0]["statements"].clone();
    assert_eq!(before[0]["error"], 1553);
    assert_eq!(before[1]["error"], 1828);
    assert_eq!(code, Some(1));

    // A global set before them: the new words, in the same two places, and
    // the same answers.
    for set in [
        "SET GLOBAL foreign_key_checks = 0",
        "SET @@global.foreign_key_checks = 0",
        "SET PERSIST foreign_key_checks = 0",
        "SET @@persist.foreign_key_checks = 0",
    ] {
        let input = format!("{set};\n{sql}");
        let (text, json, code) = text_and_json(&["--schema", schema], &input);
        for note in [
            format!(
                "orders_user_id_foreign is the only index that serves the foreign key \
                 orders_user_id_foreign; {ASSUMES_CHECKS_ON_IN_SESSION}"
            ),
            format!(
                "user_id is in the foreign key orders_user_id_foreign; \
                 {ASSUMES_CHECKS_ON_IN_SESSION}"
            ),
        ] {
            assert!(text.contains(&format!("    - {note}\n")), "{set}: {text}");
            assert!(
                json["sources"][0]["statements"].to_string().contains(&note),
                "{set}"
            );
        }
        assert!(!text.contains(ASSUMES_CHECKS_ON), "{set}: {text}");
        let after = &json["sources"][0]["statements"];
        assert_eq!(after[1]["error"], 1553, "{set}");
        assert_eq!(after[2]["error"], 1828, "{set}");
        assert_eq!(code, Some(1), "{set}");
    }
}

#[test]
fn the_binary_log_note_names_binlog_format_row_in_text_and_json() {
    // $table->uuid('token')->default(new Expression('(UUID())'))
    let schema = example("mysql-schema.sql");
    let sql = "alter table `orders` add `token` char(36) not null default (UUID());\n\
               alter table `orders` add `due_on` date not null default (CURDATE());";
    let (text, json, _) = text_and_json(&["--schema", schema.to_str().unwrap()], sql);
    assert_eq!(
        text.matches(&format!("    - {ASSUMES_BINARY_LOG}\n"))
            .count(),
        2,
        "{text}"
    );
    assert!(!text.contains(ASSUMES_BINARY_LOG_BEFORE_11A), "{text}");
    for statement in json["sources"][0]["statements"].as_array().unwrap() {
        assert_eq!(notes_of(statement)[0], ASSUMES_BINARY_LOG);
    }
}

#[test]
fn rows_put_in_a_table_before_it_is_renamed_keep_the_row_notes_of_the_new_name() {
    // The INSERT names `c`, and the rename carries the rows to `d`.
    let input = "create table `c` (`id` int not null, `code` varchar(20) null);
                 insert into `c` (`id`) values (1);
                 rename table `c` to `d`;
                 alter table `d` add unique `d_code_unique`(`code`);";
    let (text, json, _) = text_and_json(&[], input);
    assert!(
        text.contains("    - if the columns hold duplicate values: "),
        "{text}"
    );
    assert!(
        last_notes(&json)
            .iter()
            .any(|note| note.starts_with("if the columns hold duplicate values: "))
    );
}

#[test]
fn a_table_statement_kilit_cannot_read_is_unknown_and_prints_in_full_not_as_a_read_line() {
    let input = one_migration(&["select 0", "table `orders`"]);
    let text = stdout(&kilit(&["check"], &input));
    assert!(
        text.contains("\n  table `orders`\n    UNKNOWN: kilit could not read this statement"),
        "{text}"
    );
    assert!(!text.contains("read (not checked)"), "{text}");
}

// Regression tests for the review of step 11a.

const ADD_CHILDREN_KEY: &str = "alter table `children` add constraint \
     `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`);";

#[test]
fn set_persist_only_and_a_read_of_the_global_value_keep_the_old_assumption_note() {
    // PERSIST_ONLY changes neither the global nor the session value (measured
    // on 8.0.46 and 8.4.13), and a SET @saved = @@GLOBAL.x only reads it.
    for set in [
        "SET PERSIST_ONLY foreign_key_checks = 0",
        "set persist_only foreign_key_checks = 0",
        "SET @@persist_only.foreign_key_checks = 0",
        "SET @saved = @@GLOBAL.foreign_key_checks",
        "SET @saved = @@global.foreign_key_checks",
    ] {
        let input = format!("{set};\n{NEW_TABLES}{ADD_CHILDREN_KEY}\n");
        let (text, json, code) = text_and_json(&[], &input);
        assert!(
            text.contains(&format!("    - {ASSUMES_CHECKS_ON}\n")),
            "{set}: {text}"
        );
        assert!(!text.contains("in this session"), "{set}: {text}");
        assert_eq!(last_notes(&json)[0], ASSUMES_CHECKS_ON, "{set}");
        assert_eq!(code, Some(0), "{set}");
    }
}

#[test]
fn set_persist_only_before_dropping_a_foreign_key_column_keeps_the_old_words() {
    let schema = example("mysql-schema.sql");
    let schema = schema.to_str().unwrap();
    let input = "SET PERSIST_ONLY foreign_key_checks = 0;
                 alter table orders drop index orders_user_id_foreign;
                 alter table orders drop column user_id;";
    let (text, _, code) = text_and_json(&["--schema", schema], input);
    for note in [
        format!(
            "orders_user_id_foreign is the only index that serves the foreign key \
             orders_user_id_foreign; {ASSUMES_CHECKS_ON}"
        ),
        format!("user_id is in the foreign key orders_user_id_foreign; {ASSUMES_CHECKS_ON}"),
    ] {
        assert!(text.contains(&format!("    - {note}\n")), "{text}");
    }
    assert!(!text.contains("in this session"), "{text}");
    assert_eq!(code, Some(1));
}

#[test]
fn a_session_default_after_a_global_set_does_not_claim_the_global_value_changed_this_session() {
    // Step 11b: SET foreign_key_checks = DEFAULT leaves the session's checks
    // unknown (hand tests: off on both servers), so the key is unknown, with
    // the line of the SET, and the claim about the global value is gone.
    let input = format!(
        "SET GLOBAL foreign_key_checks = 0;\nSET foreign_key_checks = DEFAULT;\n\
         {NEW_TABLES}{ADD_CHILDREN_KEY}\n"
    );
    let (text, json, code) = text_and_json(&[], &input);
    assert!(!text.contains("changes its global value"), "{text}");
    assert!(
        !last_notes(&json)
            .iter()
            .any(|note| note.contains("changes its global value"))
    );
    let statement = json["sources"][0]["statements"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(statement["impact"], "unknown");
    assert_eq!(
        statement["reason"],
        "line 2 sets foreign_key_checks to DEFAULT, which the lab has not measured"
    );
    assert_eq!(code, Some(0));
}

const ROWS_IN_EV: &str = "create table `ev` (`id` int not null, `n` int null);\n";
const ADD_EV_UNIQUE: &str = "alter table `ev` add unique index `ev_n` (`n`);\n";
const DUPLICATES: &str = "if the columns hold duplicate values: ";

#[test]
fn a_comment_statement_that_the_splitter_drops_keeps_the_row_notes_of_every_new_table() {
    // mysqldump writes its inserts as /*!40000 ... */ statements. Both servers
    // run them: the unique index then fails with 1062.
    let input =
        format!("{ROWS_IN_EV}/*!40000 INSERT INTO ev VALUES (1,1),(2,1) */;\n{ADD_EV_UNIQUE}");
    let (text, json, _) = text_and_json(&[], &input);
    assert!(text.contains(&format!("    - {DUPLICATES}")), "{text}");
    assert!(
        last_notes(&json)
            .iter()
            .any(|note| note.starts_with(DUPLICATES))
    );
}

#[test]
fn a_comment_statement_in_any_input_file_keeps_the_row_notes() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("comment_statement_files");
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, content: String| {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    };
    let comment = "/*!40000 INSERT INTO ev VALUES (1,1),(2,1) */;\n";
    // The comment in the second file, before the ALTER.
    let first = write("1_create.sql", ROWS_IN_EV.to_owned());
    let second = write("2_fill_alter.sql", format!("{comment}{ADD_EV_UNIQUE}"));
    // The comment in a file after the ALTER's file.
    let alter_file = write("3_create_alter.sql", format!("{ROWS_IN_EV}{ADD_EV_UNIQUE}"));
    let last = write("4_comment.sql", comment.to_owned());
    for files in [[first, second], [alter_file, last]] {
        let mut args = vec!["check"];
        args.extend(files.iter().map(|path| path.to_str().unwrap()));
        let output = kilit(&args, "");
        let text = stdout(&output);
        assert!(
            text.contains(&format!("    - {DUPLICATES}")),
            "{files:?}: {text}"
        );
    }
}

#[test]
fn a_trigger_or_a_function_before_a_statement_keeps_the_row_notes_of_every_new_table() {
    // The trigger copies rows into ev when orders is updated; the function is
    // called by a SET. Both servers: two rows in ev, then 1062.
    for input in [
        format!(
            "CREATE TRIGGER orders_audit AFTER UPDATE ON orders FOR EACH ROW \
             INSERT INTO ev VALUES (OLD.id, 1);\n{ROWS_IN_EV}\
             UPDATE orders SET note = 'x';\n{ADD_EV_UNIQUE}"
        ),
        format!(
            "CREATE FUNCTION fill() RETURNS INT DETERMINISTIC RETURN 1;\n{ROWS_IN_EV}\
             SET @x = fill();\n{ADD_EV_UNIQUE}"
        ),
    ] {
        let (text, json, _) = text_and_json(&[], &input);
        assert!(
            text.contains(&format!("    - {DUPLICATES}")),
            "{input}: {text}"
        );
        assert!(
            last_notes(&json)
                .iter()
                .any(|note| note.starts_with(DUPLICATES)),
            "{input}"
        );
    }
}

#[test]
fn a_procedure_or_an_event_before_a_statement_keeps_the_row_notes_too() {
    for create in [
        "CREATE PROCEDURE fill_ev() INSERT INTO ev VALUES (1, 1)",
        "CREATE EVENT fill_ev_daily ON SCHEDULE EVERY 1 DAY DO INSERT INTO ev VALUES (1, 1)",
    ] {
        let input = format!("{create};\n{ROWS_IN_EV}SET @x = 1;\n{ADD_EV_UNIQUE}");
        let (text, _, _) = text_and_json(&[], &input);
        assert!(
            text.contains(&format!("    - {DUPLICATES}")),
            "{create}: {text}"
        );
    }
}

#[test]
fn a_laravel_output_with_an_unreadable_line_keeps_every_row_note_and_exits_with_2() {
    // An insert line whose mark is mangled is unreadable: it may have filled
    // `children`.
    let input = concat!(
        "\n",
        "   INFO  Running migrations.  \n",
        "\n",
        "  2026_03_02_000002_create_parents_children_fk ...............................  \n",
        "  \u{21c2} create table `parents` (`id` bigint unsigned not null auto_increment primary key)  \n",
        "  \u{21c2} create table `children` (`id` bigint unsigned not null auto_increment primary key, `parent_id` bigint unsigned not null)  \n",
        "  X insert into `children` (`parent_id`) values (1)  \n",
        "  \u{21c2} alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`)  \n",
        "\n",
    );
    let output = kilit(&["check"], input);
    let text = stdout(&output);
    assert!(
        text.contains("    - if a row points to a parent row that does not exist: "),
        "{text}"
    );
    assert!(text.contains("kilit could not read these lines"), "{text}");
    assert_eq!(output.status.code(), Some(2), "{text}");
}

#[test]
fn a_schema_qualified_table_with_an_insert_by_its_short_name_keeps_the_row_notes() {
    let input = "create table shop.events (`id` int not null, `n` int null);
                 insert into events values (1, 1), (2, 1);
                 alter table shop.events add unique index events_n (`n`);";
    let (text, json, _) = text_and_json(&[], input);
    assert!(text.contains(&format!("    - {DUPLICATES}")), "{text}");
    assert!(
        last_notes(&json)
            .iter()
            .any(|note| note.starts_with(DUPLICATES))
    );
}

#[test]
fn an_alter_kilit_reads_but_does_not_model_keeps_the_row_notes_of_the_next_alter() {
    // AUTO_INCREMENT = 5 and ALTER COLUMN ... DROP DEFAULT parse as ALTERs
    // with a change kilit does not model, and kilit answers UNKNOWN for them.
    // They are no statement kilit fully reads: the row note of the next ALTER
    // stays.
    for alter in [
        "alter table `ev` auto_increment = 5",
        "alter table `ev` alter column `n` drop default",
    ] {
        let input = format!("{ROWS_IN_EV}{alter};\n{ADD_EV_UNIQUE}");
        let (text, _, _) = text_and_json(&[], &input);
        assert!(
            text.contains("kilit does not understand this change"),
            "{alter}: {text}"
        );
        assert!(
            text.contains(&format!("    - {DUPLICATES}")),
            "{alter}: {text}"
        );
    }
}

// ---------------------------------------------------------------------------
// Step 11b: table-option ALTERs, and one session per input file.
// ---------------------------------------------------------------------------

/// The statements of source number `source` in `json`.
fn statements_of(json: &serde_json::Value, source: usize) -> Vec<serde_json::Value> {
    json["sources"][source]["statements"]
        .as_array()
        .unwrap()
        .clone()
}

/// kilit's JSON for `sql` against the example's tables, and the exit status.
fn option_check(server: &str, strict: bool, sql: &str) -> (serde_json::Value, Option<i32>) {
    json_check(Some(server), strict, sql)
}

#[test]
fn a_table_comment_is_inplace_in_json_and_exits_with_0_with_and_without_strict() {
    // $table->comment('Orders placed by users');
    let sql = "alter table `orders` comment = 'Orders placed by users';";
    for server in ["mysql-8.0.46", "mysql-8.4.13"] {
        for strict in [false, true] {
            let (json, code) = option_check(server, strict, sql);
            let statement = &statements_of(&json, 0)[0];
            assert_eq!(statement["impact"], "writes_continue", "{server}");
            assert_eq!(statement["case"], "set-table-comment", "{server}");
            assert_eq!(statement["algorithm"], "INPLACE", "{server}");
            assert_eq!(statement["rebuilds_table"], false, "{server}");
            assert_eq!(statement["copies_rows"], false, "{server}");
            assert_eq!(json["exit_code"], 0, "{server}");
            assert_eq!(code, Some(0), "{server} strict={strict}");
        }
    }
    let schema = example("mysql-schema.sql");
    let output = kilit(&["check", "--schema", schema.to_str().unwrap()], sql);
    let text = stdout(&output);
    assert!(
        text.contains(
            "  line 1: alter table `orders` comment = 'Orders placed by users'\n    INPLACE, no rebuild, writes continue\n"
        ),
        "{text}"
    );
}

#[test]
fn a_forced_algorithm_on_a_table_comment_changes_the_impact_and_the_exit_code() {
    for (clause, impact, code) in [
        (", algorithm=inplace", "writes_continue", 0),
        (", algorithm=inplace, lock=none", "writes_continue", 0),
        (", lock=none", "writes_continue", 0),
        (", algorithm=copy", "unknown", 0),
        (", algorithm=instant", "fails", 1),
    ] {
        let sql = format!("alter table `orders` comment = 'x'{clause};");
        for strict in [false, true] {
            let (json, status) = option_check("mysql-8.4.13", strict, &sql);
            let statement = &statements_of(&json, 0)[0];
            assert_eq!(statement["impact"], impact, "{clause}");
            if impact == "fails" {
                assert_eq!(statement["error"], 1845, "{clause}");
            }
            // Unknown makes only --strict exit, with 3.
            let code = if strict && impact == "unknown" {
                3
            } else {
                code
            };
            assert_eq!(json["exit_code"], code, "{clause}");
            assert_eq!(status, Some(code), "{clause} strict={strict}");
        }
    }
}

#[test]
fn an_engine_or_row_format_change_is_unknown_with_its_reason_and_only_strict_exits_with_3() {
    for (sql, reason) in [
        (
            "alter table `orders` engine = InnoDB;",
            "changing a table's ENGINE has not been measured yet",
        ),
        (
            "alter table `orders` row_format = dynamic;",
            "changing a table's ROW_FORMAT has not been measured yet",
        ),
        (
            "alter table `orders` row_format = compressed key_block_size = 8;",
            "changing a table's ROW_FORMAT and KEY_BLOCK_SIZE has not been measured yet",
        ),
    ] {
        let (json, code) = option_check("mysql-8.4.13", false, sql);
        let statement = &statements_of(&json, 0)[0];
        assert_eq!(statement["impact"], "unknown", "{sql}");
        assert_eq!(statement["reason"], reason, "{sql}");
        assert_eq!(code, Some(0), "{sql}");
        let (json, code) = option_check("mysql-8.4.13", true, sql);
        assert_eq!(json["exit_code"], 3, "{sql}");
        assert_eq!(code, Some(3), "{sql}");
    }
}

#[test]
fn a_compressed_table_made_by_an_alter_gets_the_compressed_answer_afterwards() {
    let sql = "alter table `orders` row_format = compressed;
               alter table `orders` add `coupon` varchar(255) null;";
    let (json, code) = option_check("mysql-8.4.13", false, sql);
    let statements = statements_of(&json, 0);
    assert_eq!(statements[0]["impact"], "unknown");
    assert_eq!(statements[1]["impact"], "writes_continue");
    assert_eq!(statements[1]["algorithm"], "INPLACE");
    assert_eq!(statements[1]["rebuilds_table"], true);
    assert_eq!(code, Some(0));
}

#[test]
fn laravel_s_table_comment_is_read_from_standard_input_in_both_forms() {
    // Schema::create('notes', ... $table->comment('...')) and
    // Schema::table('shops', fn ($table) => $table->comment('...')).
    let create = "create table `notes` (`id` bigint unsigned not null auto_increment primary key, `body` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'";
    let comment = "alter table `notes` comment = 'Notes kept by staff'";
    let shops = "alter table `shops` comment = 'It''s a shop'";
    for (form, input) in [
        (
            "long",
            pretend_long(&[
                (
                    "2026_04_01_000001_a_create_notes_comment",
                    &[create, comment],
                ),
                ("2026_04_01_000002_b_table_comment", &[shops]),
            ]),
        ),
        (
            "short",
            pretend_short(&[
                (
                    "2026_04_01_000001_a_create_notes_comment",
                    &[create, comment],
                ),
                ("2026_04_01_000002_b_table_comment", &[shops]),
            ]),
        ),
    ] {
        let schema = example("mysql-schema.sql");
        let output = kilit(
            &[
                "check",
                "--schema",
                schema.to_str().unwrap(),
                "--format",
                "json",
            ],
            &input,
        );
        let json = json_of(&output);
        let first = statements_of(&json, 0);
        assert_eq!(first[0]["impact"], "not_checked", "{form}");
        assert_eq!(first[1]["impact"], "new_table", "{form}");
        assert_eq!(first[1]["case"], "set-table-comment", "{form}");
        let second = statements_of(&json, 1);
        assert_eq!(second[0]["impact"], "writes_continue", "{form}");
        assert_eq!(second[0]["algorithm"], "INPLACE", "{form}");
        assert_eq!(output.status.code(), Some(0), "{form}");
    }
}

// Each input is a session of its own. `kilit check a.sql b.sql` runs like
// `mysql < a.sql` and `mysql < b.sql`: a session SET in a.sql does not reach
// b.sql, and a SET GLOBAL in a.sql is b.sql's starting value. The tables
// carry over: DDL is not per session. One --pretend output is one `artisan
// migrate`, so one session for all its migrations.

const FILE_CHECKS_NOTE: &str = "assumes foreign_key_checks is ON, the MySQL default: an earlier \
     file sets it, but each file runs in a new session, and nothing in this one sets it before \
     this statement";
const FILE_CHECKS_GLOBAL: &str = "an earlier file sets foreign_key_checks globally, and each \
     file runs in a new session, which starts with the global value";
const FILE_OLD_ALTER_NOTE: &str = "assumes old_alter_table is OFF, the MySQL default: an earlier \
     file sets it, but each file runs in a new session, and nothing in this one sets it before \
     this statement";
const FILE_OLD_ALTER_REASON: &str = "an earlier file sets old_alter_table globally or in a form \
     kilit cannot read, and each file runs in a new session, which starts with the global \
     value; with it ON MySQL copies the table";
const FILE_BINARY_LOG_NOTE: &str = "assumes the binary log is on with binlog_format = ROW, the \
     MySQL default: an earlier file sets sql_log_bin, but each file runs in a new session, and \
     nothing in this one sets it before this statement";
const FILE_BINARY_LOG_REASON: &str = "a statement before this one sets sql_log_bin globally in \
     an earlier file, and the lab has measured expression defaults only with the binary log on";

/// A column of the wrong type for a key to `users`, and the key.
const ADD_REFUND_REF: &str = "alter table orders add refund_ref int not null;";
const ADD_REFUND_KEY: &str = "alter table orders add constraint orders_refund_ref_foreign \
     foreign key (refund_ref) references users (id);";

/// Writes the files into a folder of their own and returns their paths.
fn session_files(folder: &str, files: &[(&str, &str)]) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(folder);
    std::fs::create_dir_all(&dir).unwrap();
    files
        .iter()
        .map(|(name, content)| {
            let path = dir.join(name);
            std::fs::write(&path, content).unwrap();
            path
        })
        .collect()
}

/// kilit's JSON for `files` against the example's tables, and the exit status.
fn files_json(files: &[PathBuf], extra: &[&str], stdin: &str) -> (serde_json::Value, Option<i32>) {
    let schema = example("mysql-schema.sql");
    let mut args = vec![
        "check",
        "--schema",
        schema.to_str().unwrap(),
        "--format",
        "json",
    ];
    args.extend(extra);
    args.extend(files.iter().map(|path| path.to_str().unwrap()));
    let output = kilit(&args, stdin);
    (json_of(&output), output.status.code())
}

/// The last statement of source `source`.
fn last_of(json: &serde_json::Value, source: usize) -> serde_json::Value {
    statements_of(json, source).pop().unwrap()
}

#[test]
fn a_session_set_in_one_file_does_not_reach_the_next_file() {
    let files = session_files(
        "session_checks_off",
        &[
            (
                "a.sql",
                &format!("SET foreign_key_checks = 0;\n{ADD_REFUND_REF}\n"),
            ),
            ("b.sql", &format!("{ADD_REFUND_KEY}\n")),
        ],
    );
    // The key is checked with the checks on, as MySQL does in the new
    // session; the tables carried over (refund_ref is an int).
    for strict in [false, true] {
        let extra: &[&str] = if strict { &["--strict"] } else { &[] };
        let (json, code) = files_json(&files, extra, "");
        let key = last_of(&json, 1);
        assert_eq!(key["impact"], "fails");
        assert_eq!(key["error"], 3780);
        assert!(
            notes_of(&key).contains(&FILE_CHECKS_NOTE.to_owned()),
            "{key}"
        );
        assert!(
            !notes_of(&key).iter().any(|note| note == ASSUMES_CHECKS_ON),
            "{key}"
        );
        assert_eq!(json["exit_code"], 1);
        assert_eq!(code, Some(1));
    }
    let mut args = vec!["check", "--schema"];
    let schema = example("mysql-schema.sql");
    args.push(schema.to_str().unwrap());
    args.extend(files.iter().map(|path| path.to_str().unwrap()));
    let text = stdout(&kilit(&args, ""));
    assert!(
        text.contains(&format!("    - {FILE_CHECKS_NOTE}\n")),
        "{text}"
    );
    // The same two files joined into one are one session: the checks are off,
    // the key's mismatch is something kilit can see, and the answer is unknown.
    let joined = session_files(
        "session_checks_off_joined",
        &[(
            "ab.sql",
            &format!("SET foreign_key_checks = 0;\n{ADD_REFUND_REF}\n{ADD_REFUND_KEY}\n"),
        )],
    );
    let (json, _) = files_json(&joined, &[], "");
    let key = last_of(&json, 0);
    assert_eq!(key["impact"], "unknown");
    assert!(
        !notes_of(&key).contains(&FILE_CHECKS_NOTE.to_owned()),
        "{key}"
    );
}

#[test]
fn a_set_at_the_very_end_of_a_file_does_not_reach_the_next_file() {
    let files = session_files(
        "session_checks_off_at_the_end",
        &[
            (
                "a.sql",
                &format!("{ADD_REFUND_REF}\nSET foreign_key_checks = 0"),
            ),
            ("b.sql", &format!("{ADD_REFUND_KEY}\n")),
        ],
    );
    let (json, _) = files_json(&files, &[], "");
    let key = last_of(&json, 1);
    assert_eq!(key["error"], 3780);
    assert!(
        notes_of(&key).contains(&FILE_CHECKS_NOTE.to_owned()),
        "{key}"
    );
}

#[test]
fn a_file_that_sets_the_checks_again_is_as_before() {
    let files = session_files(
        "session_checks_set_again",
        &[
            (
                "a.sql",
                &format!("SET foreign_key_checks = 0;\n{ADD_REFUND_REF}\n"),
            ),
            (
                "b.sql",
                &format!("SET foreign_key_checks = 1;\n{ADD_REFUND_KEY}\n"),
            ),
        ],
    );
    let (json, _) = files_json(&files, &[], "");
    let key = last_of(&json, 1);
    assert_eq!(key["error"], 3780);
    // Nothing is assumed: this file sets them itself.
    assert!(
        !notes_of(&key)
            .iter()
            .any(|note| note.contains("assumes foreign_key_checks")),
        "{key}"
    );
}

#[test]
fn a_global_foreign_key_checks_in_one_file_makes_the_checks_unknown_in_every_later_file() {
    for set in [
        "SET GLOBAL foreign_key_checks = 0",
        "SET PERSIST foreign_key_checks = 0",
        "SET @@global.foreign_key_checks = 0",
    ] {
        let files = session_files(
            "session_checks_global",
            &[
                ("a.sql", &format!("{set};\n{ADD_REFUND_REF}\n")),
                ("b.sql", &format!("{ADD_REFUND_KEY}\n")),
                (
                    "c.sql",
                    "alter table orders add other_ref int not null;
                     alter table orders add constraint orders_other_ref_foreign \
                     foreign key (other_ref) references users (id);\n",
                ),
                (
                    "d.sql",
                    "SET foreign_key_checks = 1;\nalter table orders add constraint \
                     orders_refund_ref_again foreign key (refund_ref) references users (id);\n",
                ),
            ],
        );
        for strict in [false, true] {
            let extra: &[&str] = if strict { &["--strict"] } else { &[] };
            let (json, code) = files_json(&files, extra, "");
            for source in [1, 2] {
                let key = last_of(&json, source);
                assert_eq!(key["impact"], "unknown", "{set}: file {source}");
                assert_eq!(key["reason"], FILE_CHECKS_GLOBAL, "{set}: file {source}");
            }
            // A session SET in the file brings the measured answer back.
            let key = last_of(&json, 3);
            assert_eq!(key["impact"], "fails", "{set}");
            assert_eq!(key["error"], 3780, "{set}");
            assert_eq!(code, Some(1), "{set}");
        }
    }
    // Nothing fails: only unknown, and only strict exits, with 3.
    let files = session_files(
        "session_checks_global_only_unknown",
        &[
            (
                "a.sql",
                &format!("SET GLOBAL foreign_key_checks = 0;\n{ADD_REFUND_REF}\n"),
            ),
            (
                "b.sql",
                "alter table orders add constraint orders_refund_ref_foreign foreign key \
                 (user_id) references users (id);\n",
            ),
        ],
    );
    let (json, code) = files_json(&files, &[], "");
    assert_eq!(last_of(&json, 1)["impact"], "unknown");
    assert_eq!(code, Some(0));
    let (_, code) = files_json(&files, &["--strict"], "");
    assert_eq!(code, Some(3));
}

#[test]
fn old_alter_table_on_in_one_file_is_off_in_the_next_and_the_note_says_so() {
    let files = session_files(
        "session_old_alter_table",
        &[
            ("a.sql", "SET old_alter_table = ON;\n"),
            (
                "b.sql",
                "alter table orders add coupon varchar(255) null;
                 alter table orders add coupon2 varchar(255) null, algorithm=instant;\n",
            ),
        ],
    );
    let (json, code) = files_json(&files, &[], "");
    let statements = statements_of(&json, 1);
    // The normal answer, with a note.
    assert_eq!(statements[0]["impact"], "writes_continue");
    assert_eq!(statements[0]["algorithm"], "INSTANT");
    assert!(
        notes_of(&statements[0]).contains(&FILE_OLD_ALTER_NOTE.to_owned()),
        "{}",
        statements[0]
    );
    // A written ALGORITHM wins over the setting: no note.
    assert_eq!(statements[1]["algorithm"], "INSTANT");
    assert!(
        !notes_of(&statements[1])
            .iter()
            .any(|note| note.contains("old_alter_table")),
        "{}",
        statements[1]
    );
    assert_eq!(code, Some(0));
}

#[test]
fn old_alter_table_set_globally_or_unreadably_in_one_file_is_unknown_in_the_next() {
    for set in [
        "SET GLOBAL old_alter_table = ON",
        "SET PERSIST old_alter_table = ON",
        "SET @@global.old_alter_table = ON",
    ] {
        let files = session_files(
            "session_old_alter_table_global",
            &[
                ("a.sql", &format!("{set};\n")),
                (
                    "b.sql",
                    "alter table orders add coupon varchar(255) null;\n",
                ),
            ],
        );
        let (json, code) = files_json(&files, &[], "");
        let statement = last_of(&json, 1);
        assert_eq!(statement["impact"], "unknown", "{set}");
        assert_eq!(statement["reason"], FILE_OLD_ALTER_REASON, "{set}");
        assert_eq!(code, Some(0), "{set}");
        let (_, code) = files_json(&files, &["--strict"], "");
        assert_eq!(code, Some(3), "{set}");
    }
}

#[test]
fn sql_log_bin_set_in_one_file_changes_the_binary_log_note_of_the_next() {
    let default = "alter table orders add token char(36) not null default (UUID());\n";
    let files = session_files(
        "session_sql_log_bin",
        &[("a.sql", "SET sql_log_bin = 0;\n"), ("b.sql", default)],
    );
    let (json, code) = files_json(&files, &[], "");
    let statement = last_of(&json, 1);
    assert_eq!(statement["impact"], "fails");
    assert_eq!(statement["error"], 1674);
    assert!(
        notes_of(&statement).contains(&FILE_BINARY_LOG_NOTE.to_owned()),
        "{statement}"
    );
    assert!(
        !notes_of(&statement).contains(&ASSUMES_BINARY_LOG.to_owned()),
        "{statement}"
    );
    assert_eq!(code, Some(1));
    // Without an earlier file's SET, the plain note.
    let (json, _) = files_json(
        &session_files("session_sql_log_bin_plain", &[("b.sql", default)]),
        &[],
        "",
    );
    assert!(
        notes_of(&last_of(&json, 0)).contains(&ASSUMES_BINARY_LOG.to_owned()),
        "{json}"
    );
    // A GLOBAL one makes the expression default unknown in the next file.
    let files = session_files(
        "session_sql_log_bin_global",
        &[
            ("a.sql", "SET GLOBAL sql_log_bin = 1;\n"),
            ("b.sql", default),
        ],
    );
    let (json, code) = files_json(&files, &[], "");
    let statement = last_of(&json, 1);
    assert_eq!(statement["impact"], "unknown");
    assert_eq!(statement["reason"], FILE_BINARY_LOG_REASON);
    assert_eq!(code, Some(0));
}

#[test]
fn settings_that_are_scanned_anywhere_earlier_keep_looking_across_files() {
    let uuid = "alter table orders add token char(36) not null default (UUID());\n";
    // binlog_format: the lab measured the expression defaults with ROW.
    let files = session_files(
        "session_binlog_format",
        &[
            ("a.sql", "SET binlog_format = 'STATEMENT';\n"),
            ("b.sql", uuid),
        ],
    );
    let (json, _) = files_json(&files, &[], "");
    let statement = last_of(&json, 1);
    assert_eq!(statement["impact"], "unknown");
    assert!(
        statement["reason"]
            .as_str()
            .unwrap()
            .contains("binlog_format"),
        "{statement}"
    );
    // restrict_fk_on_non_standard_key: the index rules are unknown.
    let files = session_files(
        "session_restrict_fk",
        &[
            ("a.sql", "SET restrict_fk_on_non_standard_key = OFF;\n"),
            ("b.sql", FOREIGN_KEY_TO_NAME),
        ],
    );
    let (json, _) = files_json(&files, &[], "");
    assert_eq!(last_of(&json, 1)["impact"], "unknown");
    // innodb_default_row_format: a table without a row format is unknown.
    let files = session_files(
        "session_row_format",
        &[
            ("a.sql", "SET GLOBAL innodb_default_row_format = COMPACT;\n"),
            (
                "b.sql",
                "alter table orders add coupon varchar(255) null;\n",
            ),
        ],
    );
    let (json, _) = files_json(&files, &[], "");
    assert_eq!(last_of(&json, 1)["impact"], "unknown");
}

#[test]
fn standard_input_after_a_file_is_a_session_of_its_own() {
    let files = session_files(
        "session_stdin",
        &[(
            "a.sql",
            &format!("SET foreign_key_checks = 0;\n{ADD_REFUND_REF}\n"),
        )],
    );
    let mut all = files.clone();
    all.push(PathBuf::from("-"));
    let (json, _) = files_json(&all, &[], &format!("{ADD_REFUND_KEY}\n"));
    assert_eq!(json["sources"].as_array().unwrap().len(), 2);
    let key = last_of(&json, 1);
    assert_eq!(key["error"], 3780);
    assert!(
        notes_of(&key).contains(&FILE_CHECKS_NOTE.to_owned()),
        "{key}"
    );
}

#[test]
fn a_pretend_file_and_a_sql_file_are_two_sessions() {
    let pretend = pretend_long(&[(
        "2026_04_02_000001_add_refund_ref_to_orders_table",
        &[
            "SET FOREIGN_KEY_CHECKS=0;",
            "alter table `orders` add `refund_ref` int not null",
        ],
    )]);
    let files = session_files(
        "session_pretend_then_sql",
        &[
            ("1_pretend.txt", &pretend),
            ("2_key.sql", &format!("{ADD_REFUND_KEY}\n")),
        ],
    );
    let (json, _) = files_json(&files, &[], "");
    let key = last_of(&json, 1);
    assert_eq!(key["error"], 3780);
    assert!(
        notes_of(&key).contains(&FILE_CHECKS_NOTE.to_owned()),
        "{key}"
    );
    // And the other way round: a SQL file, then Laravel's output.
    let pretend_key = pretend_long(&[(
        "2026_04_02_000002_add_refund_key_to_orders_table",
        &[
            "alter table `orders` add constraint `orders_refund_ref_foreign` foreign key (`refund_ref`) references `users` (`id`)",
        ],
    )]);
    let files = session_files(
        "session_sql_then_pretend",
        &[
            (
                "1_set.sql",
                &format!("SET foreign_key_checks = 0;\n{ADD_REFUND_REF}\n"),
            ),
            ("2_pretend.txt", &pretend_key),
        ],
    );
    let (json, _) = files_json(&files, &[], "");
    let key = last_of(&json, 1);
    assert_eq!(key["error"], 3780);
    assert!(
        notes_of(&key).contains(&FILE_CHECKS_NOTE.to_owned()),
        "{key}"
    );
}

#[test]
fn two_migrations_in_one_pretend_file_are_one_session() {
    // Schema::disableForeignKeyConstraints() in the first migration is still
    // in force in the second: one `artisan migrate`, one connection.
    let pretend = pretend_long(&[
        (
            "2026_04_02_000001_add_refund_ref_to_orders_table",
            &[
                "SET FOREIGN_KEY_CHECKS=0;",
                "alter table `orders` add `refund_ref` int not null",
            ],
        ),
        (
            "2026_04_02_000002_add_refund_key_to_orders_table",
            &[
                "alter table `orders` add constraint `orders_refund_ref_foreign` foreign key (`refund_ref`) references `users` (`id`)",
            ],
        ),
    ]);
    let files = session_files("session_pretend_two_migrations", &[("p.txt", &pretend)]);
    let (json, _) = files_json(&files, &[], "");
    let key = last_of(&json, 1);
    // The checks are off and the mismatch is visible: unknown, not a failure,
    // and no note about an earlier file.
    assert_eq!(key["impact"], "unknown");
    assert!(
        !notes_of(&key).contains(&FILE_CHECKS_NOTE.to_owned()),
        "{key}"
    );
}

#[test]
fn the_tables_carry_over_from_one_file_to_the_next() {
    let files = session_files(
        "session_tables_carry_over",
        &[
            (
                "a.sql",
                "create table refunds (id bigint unsigned not null auto_increment primary key, \
                 order_ref int not null);\n",
            ),
            (
                "b.sql",
                "alter table refunds add constraint refunds_order_ref_foreign foreign key \
                 (order_ref) references orders (id);\n",
            ),
        ],
    );
    let (json, code) = files_json(&files, &[], "");
    let key = last_of(&json, 1);
    assert_eq!(key["impact"], "fails");
    assert_eq!(key["error"], 3780);
    assert_eq!(code, Some(1));
}

#[test]
fn the_help_of_check_says_each_file_runs_in_a_session_of_its_own() {
    let output = kilit(&["check", "--help"], "");
    let help = stdout(&output)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(help.contains("in a session of its own"), "{help}");
    assert_eq!(output.status.code(), Some(0));
}

// Regression tests for what the step 11b review found.

#[test]
fn a_global_set_with_a_comment_after_set_still_reaches_the_next_file() {
    let files = session_files(
        "session_checks_global_comment",
        &[
            (
                "a.sql",
                &format!("SET/**/GLOBAL foreign_key_checks = 0;\n{ADD_REFUND_REF}\n"),
            ),
            ("b.sql", &format!("{ADD_REFUND_KEY}\n")),
        ],
    );
    let (json, _) = files_json(&files, &[], "");
    let key = last_of(&json, 1);
    assert_eq!(key["impact"], "unknown");
    assert_eq!(key["reason"], FILE_CHECKS_GLOBAL);
}

#[test]
fn a_temporary_table_ends_with_its_file_s_session() {
    let schema = session_files(
        "temporary_schema",
        &[(
            "schema.sql",
            "CREATE TABLE users (
    id   BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    name VARCHAR(100) NOT NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
CREATE TABLE orders (
    id      BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY,
    user_id BIGINT UNSIGNED NOT NULL,
    amount  INT NOT NULL,
    email   VARCHAR(255) NULL,
    note    VARCHAR(50) NULL,
    CONSTRAINT orders_user FOREIGN KEY (user_id) REFERENCES users (id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
",
        )],
    );
    let create = "CREATE TEMPORARY TABLE orders (id INT PRIMARY KEY, v INT) ENGINE=InnoDB;\n";
    let drop = "ALTER TABLE orders DROP COLUMN user_id;\n";
    let run = |folder: &str, files: &[(&str, &str)]| {
        let paths = session_files(folder, files);
        let mut args = vec!["check", "--format", "json", "--schema"];
        args.push(schema[0].to_str().unwrap());
        args.extend(paths.iter().map(|path| path.to_str().unwrap()));
        json_of(&kilit(&args, ""))
    };
    // td.sql alone, and after tc.sql: the real orders, 1828.
    let alone = run("temporary_alone", &[("td.sql", drop)]);
    let after = run(
        "temporary_two_files",
        &[("tc.sql", create), ("td.sql", drop)],
    );
    for (json, source) in [(&alone, 0), (&after, 1)] {
        let statement = last_of(json, source);
        assert_eq!(statement["impact"], "fails", "{statement}");
        assert_eq!(statement["error"], 1828, "{statement}");
        assert_eq!(statement["case"], "drop-column", "{statement}");
    }
    // In one file the temporary table still hides the real one.
    let one = run(
        "temporary_one_file",
        &[("t.sql", &format!("{create}{drop}"))],
    );
    let statement = last_of(&one, 0);
    assert_ne!(statement["error"], 1828, "{statement}");
}

// ---------------------------------------------------------------------------
// Step 12: what kilit promises to the programs that use it. Exit statuses 0
// to 3 and --fail-on, which server an answer is for and what the other
// measured server says, the server version a schema dump names, the JSON's
// format version, and the file a Laravel error belongs to.
// ---------------------------------------------------------------------------

/// Runs on every server: writes continue.
const RUNS: &str = "alter table `orders` add `coupon` varchar(255) null;";
/// Fails with error 1674 on every server.
const FAILS: &str =
    "alter table `orders` add `ref` bigint unsigned not null default (UUID_SHORT());";
/// COPY: writes wait.
const WAITS: &str = "alter table `orders` modify `amount` bigint not null;";
/// MySQL rejects NOT VALID with error 1064: kilit cannot tell.
const UNKNOWN: &str =
    "alter table `orders` add constraint `orders_amount_check` check (`amount` > 0) not valid;";
/// Lines of `migrate --pretend` output that kilit cannot read.
const UNREADABLE: &str = "   INFO  Running migrations.

  2026_11_02_090100_widen_amount_on_orders_table .............................

   ERROR  [2026_11_02_090100_widen_amount_on_orders_table] failed to dump queries.
";

/// kilit against the example's tables.
fn on_example_tables(args: &[&str], input: &str) -> Output {
    let schema = example("mysql-schema.sql");
    let mut all = vec!["check", "--schema", schema.to_str().unwrap()];
    all.extend(args);
    kilit(&all, input)
}

/// The exit status of kilit against the example's tables.
fn status_on_example_tables(args: &[&str], input: &str) -> Option<i32> {
    on_example_tables(args, input).status.code()
}

/// kilit against the example's tables, over `files`.
fn files_on_example_tables(files: &[PathBuf], args: &[&str]) -> Output {
    let schema = example("mysql-schema.sql");
    let mut all = vec!["check", "--schema", schema.to_str().unwrap()];
    all.extend(args);
    all.extend(files.iter().map(|path| path.to_str().unwrap()));
    kilit(&all, "")
}

#[test]
fn every_exit_status_means_what_the_help_says() {
    // 0: nothing that --fail-on counts was found.
    assert_eq!(status_on_example_tables(&[], RUNS), Some(0));
    // 1: a statement fails, or makes writes wait.
    assert_eq!(status_on_example_tables(&[], FAILS), Some(1));
    assert_eq!(status_on_example_tables(&[], WAITS), Some(1));
    // 2: kilit could not check everything.
    assert_eq!(status_on_example_tables(&[], UNREADABLE), Some(2));
    assert_eq!(status_on_example_tables(&[], C1_THROW_L12), Some(2));
    assert_eq!(status_on_example_tables(&["--nope"], RUNS), Some(2));
    // 3: the only statements --fail-on counts are unknown.
    assert_eq!(status_on_example_tables(&["--strict"], UNKNOWN), Some(3));
    assert_eq!(
        status_on_example_tables(&["--fail-on", "unknown"], UNKNOWN),
        Some(3)
    );
}

#[test]
fn the_json_exit_code_is_the_exit_status_for_each_of_them() {
    for (args, input, code) in [
        (&[][..], RUNS, 0),
        (&[][..], FAILS, 1),
        (&[][..], WAITS, 1),
        (&[][..], UNKNOWN, 0),
        (&["--strict"][..], UNKNOWN, 3),
        (&[][..], UNREADABLE, 2),
        (&["--strict"][..], C1_THROW_L12, 2),
    ] {
        let mut all = vec!["--format", "json"];
        all.extend(args);
        let output = on_example_tables(&all, input);
        assert_eq!(json_of(&output)["exit_code"], code, "{args:?}: {input}");
        assert_eq!(output.status.code(), Some(code), "{args:?}: {input}");
    }
}

#[test]
fn when_several_things_apply_2_wins_over_1_and_1_over_3() {
    let files = session_files(
        "step12_precedence",
        &[
            ("fails.sql", FAILS),
            ("unknown.sql", UNKNOWN),
            ("both.sql", &format!("{FAILS}\n{UNKNOWN}")),
            ("waits_and_unknown.sql", &format!("{WAITS}\n{UNKNOWN}")),
            ("error.txt", C1_THROW_L12),
            ("unreadable.txt", UNREADABLE),
        ],
    );
    let [fails, unknown, both, waits_and_unknown, error, unreadable] = files.as_slice() else {
        unreachable!()
    };
    for (inputs, args, code) in [
        // 2 and 1: an error screen, or lines kilit cannot read, after a
        // failure.
        (vec![fails, error], vec![], 2),
        (vec![fails, unreadable], vec![], 2),
        // 2 and 3.
        (vec![unknown, error], vec!["--strict"], 2),
        (vec![unknown, unreadable], vec!["--strict"], 2),
        // 1 and 3: the failure wins, with --strict as well.
        (vec![both], vec!["--strict"], 1),
        (vec![fails, unknown], vec!["--strict"], 1),
        (vec![waits_and_unknown], vec!["--strict"], 1),
        // 2, 1 and 3 together.
        (vec![both, error], vec!["--strict"], 2),
        (
            vec![both, unreadable],
            vec!["--fail-on", "fails,waits,unknown"],
            2,
        ),
        // 3 alone, and 1 when the failure is counted too.
        (vec![unknown], vec!["--strict"], 3),
        (vec![unknown], vec!["--fail-on", "unknown"], 3),
        (vec![both], vec!["--fail-on", "unknown"], 3),
        (vec![both], vec!["--fail-on", "fails,unknown"], 1),
        // 2 also when --fail-on counts nothing that was found.
        (vec![unknown, error], vec!["--fail-on", "fails"], 2),
        (vec![unknown, error], vec![], 2),
    ] {
        let inputs: Vec<PathBuf> = inputs.into_iter().cloned().collect();
        let output = files_on_example_tables(&inputs, &args);
        assert_eq!(
            output.status.code(),
            Some(code),
            "{inputs:?} {args:?}: {}",
            stdout(&output)
        );
        let mut json_args = vec!["--format", "json"];
        json_args.extend(&args);
        let output = files_on_example_tables(&inputs, &json_args);
        assert_eq!(json_of(&output)["exit_code"], code, "{inputs:?} {args:?}");
    }
}

#[test]
fn each_fail_on_list_counts_what_it_names_and_nothing_else() {
    // (--fail-on arguments, exit status for a run that only fails, only makes
    // writes wait, only has an unknown statement)
    let table: [(&[&str], i32, i32, i32); 13] = [
        (&[], 1, 1, 0),
        (&["--fail-on", "fails"], 1, 0, 0),
        (&["--fail-on", "waits"], 0, 1, 0),
        (&["--fail-on", "unknown"], 0, 0, 3),
        (&["--fail-on", "fails,waits"], 1, 1, 0),
        (&["--fail-on", "fails,unknown"], 1, 0, 3),
        (&["--fail-on", "waits,unknown"], 0, 1, 3),
        (&["--fail-on", "unknown,waits,fails"], 1, 1, 3),
        (&["--fail-on", "fails,waits,unknown"], 1, 1, 3),
        // A repeated flag adds to the list.
        (&["--fail-on", "fails", "--fail-on", "unknown"], 1, 0, 3),
        (&["--fail-on", "waits", "--fail-on", "waits"], 0, 1, 0),
        // The same word twice counts once.
        (&["--fail-on", "fails,fails"], 1, 0, 0),
        (&["--strict"], 1, 1, 3),
    ];
    for (args, fails, waits, unknown) in table {
        for (input, code) in [(FAILS, fails), (WAITS, waits), (UNKNOWN, unknown)] {
            assert_eq!(
                status_on_example_tables(args, input),
                Some(code),
                "{args:?}: {input}"
            );
        }
        // A run that is clean passes whatever is counted.
        assert_eq!(status_on_example_tables(args, RUNS), Some(0), "{args:?}");
    }
}

#[test]
fn an_unknown_statement_next_to_a_wait_exits_with_1_and_with_3_only_when_the_wait_is_not_counted() {
    let both = format!("{WAITS}\n{UNKNOWN}");
    for (args, code) in [
        (vec![], 1),
        (vec!["--strict"], 1),
        (vec!["--fail-on", "waits"], 1),
        (vec!["--fail-on", "fails,unknown"], 3),
        (vec!["--fail-on", "unknown"], 3),
        (vec!["--fail-on", "fails"], 0),
    ] {
        assert_eq!(
            status_on_example_tables(&args, &both),
            Some(code),
            "{args:?}"
        );
    }
}

#[test]
fn a_bad_fail_on_exits_with_2_and_says_which_words_are_valid() {
    for (value, word) in [
        ("fail", Some("fail")),
        ("foo", Some("foo")),
        ("fails,foo", Some("foo")),
        ("fails,waits,everything", Some("everything")),
        // Words are lower case.
        ("Fails", Some("Fails")),
        ("FAILS,waits", Some("FAILS")),
        // An empty list, and an empty word at the end of one.
        ("", None),
        ("fails,", None),
        ("waits,unknown,", None),
    ] {
        let output = on_example_tables(&["--fail-on", value], RUNS);
        assert_eq!(output.status.code(), Some(2), "{value:?}");
        assert_eq!(stdout(&output), "", "{value:?}");
        let message = stderr(&output);
        assert!(!message.is_empty(), "{value:?}");
        if let Some(word) = word {
            assert!(message.contains(word), "{value:?}: {message}");
            for valid in ["fails", "waits", "unknown"] {
                assert!(message.contains(valid), "{value:?}: {message}");
            }
        }
        // The same with the other spelling of the option.
        let output = on_example_tables(&[&format!("--fail-on={value}")], RUNS);
        assert_eq!(output.status.code(), Some(2), "{value:?}");
        assert_eq!(stdout(&output), "", "{value:?}");
    }
    // A missing value.
    let output = on_example_tables(&["--fail-on"], RUNS);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout(&output), "");
}

#[test]
fn strict_and_fail_on_together_exit_with_2_in_either_order() {
    for args in [
        &["--strict", "--fail-on", "fails"][..],
        &["--fail-on", "fails", "--strict"][..],
        &["--fail-on", "fails,waits,unknown", "--strict"][..],
        &["--strict", "--fail-on=unknown"][..],
    ] {
        for input in [RUNS, FAILS, UNKNOWN] {
            let output = on_example_tables(args, input);
            assert_eq!(output.status.code(), Some(2), "{args:?}");
            assert_eq!(stdout(&output), "", "{args:?}");
            assert!(stderr(&output).contains("--strict"), "{args:?}");
            assert!(stderr(&output).contains("--fail-on"), "{args:?}");
        }
    }
}

#[test]
fn the_json_names_what_was_counted_in_fail_on() {
    let counted = |args: &[&str]| {
        let mut all = vec!["--format", "json"];
        all.extend(args);
        let json = json_of(&on_example_tables(&all, RUNS));
        let mut words: Vec<String> = json["fail_on"]
            .as_array()
            .unwrap()
            .iter()
            .map(|word| word.as_str().unwrap().to_owned())
            .collect();
        words.sort();
        words
    };
    assert_eq!(counted(&[]), ["fails", "waits"]);
    assert_eq!(counted(&["--strict"]), ["fails", "unknown", "waits"]);
    assert_eq!(counted(&["--fail-on", "fails"]), ["fails"]);
    assert_eq!(counted(&["--fail-on", "waits"]), ["waits"]);
    assert_eq!(counted(&["--fail-on", "unknown"]), ["unknown"]);
    assert_eq!(
        counted(&["--fail-on", "unknown,fails"]),
        ["fails", "unknown"]
    );
    // A word given twice is listed once.
    assert_eq!(counted(&["--fail-on", "waits,waits"]), ["waits"]);
    assert_eq!(
        counted(&["--fail-on", "fails", "--fail-on", "waits"]),
        ["fails", "waits"]
    );
    // The list is printed even when the run is an error screen.
    let json = json_of(&on_example_tables(&["--format", "json"], C1_THROW_L12));
    assert_eq!(json["fail_on"], serde_json::json!(["fails", "waits"]));
    assert_eq!(json["exit_code"], 2);
}

#[test]
fn the_json_has_a_format_version_first_and_the_counted_list_just_before_the_exit_code() {
    for (args, input) in [
        (vec![], RUNS),
        (vec![], FAILS),
        (vec!["--strict"], UNKNOWN),
        (vec![], C1_THROW_L12),
    ] {
        let mut all = vec!["--format", "json"];
        all.extend(&args);
        let output = on_example_tables(&all, input);
        let text = stdout(&output);
        assert!(
            text.starts_with("{\n  \"format_version\": \"1.0\",\n"),
            "{text}"
        );
        let fail_on = text.find("\n  \"fail_on\": [").expect(&text);
        let exit_code = text.find("\n  \"exit_code\": ").expect(&text);
        assert!(text[fail_on..exit_code].ends_with("],"), "{text}");
        assert!(
            !text[fail_on + 1..exit_code].contains("\n  \""),
            "something sits between fail_on and exit_code: {text}"
        );
        assert!(text.trim_end().ends_with("}"), "{text}");
        assert_eq!(json_of(&output)["format_version"], "1.0");
    }
}

#[test]
fn a_passing_run_with_unknown_statements_ends_with_a_line_that_says_how_to_count_them() {
    let one = stdout(&on_example_tables(&[], UNKNOWN));
    assert!(
        one.ends_with(
            "\n1 statement: 1 unknown\n\
             1 unknown statement is not counted in the exit status; \
             --fail-on fails,waits,unknown counts it\n"
        ),
        "{one}"
    );
    let two = stdout(&on_example_tables(&[], &format!("{UNKNOWN}\n{UNKNOWN}")));
    assert!(
        two.ends_with(
            "\n2 statements: 2 unknown\n\
             2 unknown statements are not counted in the exit status; \
             --fail-on fails,waits,unknown counts them\n"
        ),
        "{two}"
    );
    // Unknown statements next to ones that pass.
    let mixed = stdout(&on_example_tables(&[], &format!("{RUNS}\n{UNKNOWN}")));
    assert!(
        mixed.ends_with(
            "1 unknown statement is not counted in the exit status; \
             --fail-on fails,waits,unknown counts it\n"
        ),
        "{mixed}"
    );
}

#[test]
fn the_closing_line_lists_the_counted_words_once_in_the_order_fails_waits() {
    for (args, list) in [
        (vec!["--fail-on", "fails"], "fails,unknown"),
        (vec!["--fail-on", "waits"], "waits,unknown"),
        (vec!["--fail-on", "waits,fails"], "fails,waits,unknown"),
        (
            vec!["--fail-on", "waits,fails,waits,fails"],
            "fails,waits,unknown",
        ),
        (
            vec!["--fail-on", "waits", "--fail-on", "fails"],
            "fails,waits,unknown",
        ),
    ] {
        let text = stdout(&on_example_tables(&args, UNKNOWN));
        assert!(
            text.ends_with(&format!(
                "1 unknown statement is not counted in the exit status; \
                 --fail-on {list} counts it\n"
            )),
            "{args:?}: {text}"
        );
    }
}

#[test]
fn the_closing_line_is_absent_when_the_run_does_not_exit_with_0_or_nothing_is_unknown() {
    let line = "not counted in the exit status";
    let mixed = |sql: &str| format!("{sql}\n{UNKNOWN}");
    for (args, input, why) in [
        // Unknown statements are counted: exit 3.
        (vec!["--strict"], UNKNOWN.to_owned(), "strict"),
        (
            vec!["--fail-on", "unknown"],
            UNKNOWN.to_owned(),
            "unknown listed",
        ),
        (
            vec!["--fail-on", "fails,unknown"],
            UNKNOWN.to_owned(),
            "unknown listed",
        ),
        // A failure or a wait: exit 1.
        (vec![], mixed(FAILS), "a failure"),
        (vec![], mixed(WAITS), "a wait"),
        // Nothing unknown.
        (vec![], RUNS.to_owned(), "clean"),
        (vec![], FAILS.to_owned(), "only a failure"),
        // Exit 2.
        (vec![], mixed(UNREADABLE), "unreadable lines"),
        (vec![], C1_THROW_L12.to_owned(), "an error screen"),
    ] {
        let output = on_example_tables(&args, &input);
        let text = stdout(&output);
        assert!(!text.contains(line), "{why}: {text}");
    }
    // Not even when the unknown statements come before an error screen.
    let files = session_files(
        "step12_closing_line_error",
        &[("unknown.sql", UNKNOWN), ("error.txt", C1_THROW_L12)],
    );
    let text = stdout(&files_on_example_tables(&files, &[]));
    assert!(!text.contains(line), "{text}");
    // And not in the JSON, which has the counts and the exit code.
    let json = stdout(&on_example_tables(&["--format", "json"], UNKNOWN));
    assert!(!json.contains(line), "{json}");
}

// The servers.

#[test]
fn without_server_kilit_checks_against_the_mysql_84_series() {
    let text = stdout(&kilit(&["check"], RUNS));
    assert!(
        text.starts_with("checked against mysql-8.4.13, as measured by kilit-lab\n"),
        "{text}"
    );
    let json = json_of(&kilit(&["check", "--format", "json"], RUNS));
    assert_eq!(json["server"], "mysql-8.4.13");
}

#[test]
fn a_series_names_the_newest_release_the_lab_measured_in_it() {
    for (name, release) in [
        ("mysql-8.4", "mysql-8.4.13"),
        ("mysql-8.0", "mysql-8.0.46"),
        ("mysql-8.4.13", "mysql-8.4.13"),
        ("mysql-8.0.46", "mysql-8.0.46"),
    ] {
        let output = kilit(&["check", "--server", name], RUNS);
        let text = stdout(&output);
        assert!(
            text.starts_with(&format!(
                "checked against {release}, as measured by kilit-lab\n"
            )),
            "{name}: {text}"
        );
        assert_eq!(output.status.code(), Some(0), "{name}");
        let json = json_of(&kilit(
            &["check", "--server", name, "--format", "json"],
            RUNS,
        ));
        assert_eq!(json["server"], release, "{name}");
        // The equals form too.
        let text = stdout(&kilit(&["check", &format!("--server={name}")], RUNS));
        assert!(
            text.starts_with(&format!("checked against {release}, ")),
            "{name}: {text}"
        );
    }
}

#[test]
fn a_series_gives_the_answers_of_its_release() {
    // A foreign key to a parent column without an index: 1822 on 8.0, 6125
    // on 8.4.
    let schema = example("mysql-schema.sql");
    let schema = schema.to_str().unwrap();
    for (server, code) in [
        ("mysql-8.0", 1822),
        ("mysql-8.0.46", 1822),
        ("mysql-8.4", 6125),
        ("mysql-8.4.13", 6125),
    ] {
        let output = kilit(
            &["check", "--schema", schema, "--server", server],
            FOREIGN_KEY_TO_NAME,
        );
        let text = stdout(&output);
        assert!(
            text.contains(&format!("\n    fails with error {code}\n")),
            "{server}: {text}"
        );
    }
}

#[test]
fn a_server_name_that_is_neither_a_release_nor_a_series_exits_with_2() {
    for name in [
        "mysql-8",
        "mysql-8.",
        "mysql-8.4.",
        "mysql-8.4.12",
        "mysql-8.4.130",
        "8.4",
        "8.4.13",
        "mysql",
        "mysql-5.7.44",
        "mysql-9.7",
        "mariadb-10.11",
        "MySQL-8.4",
        "",
    ] {
        let output = kilit(&["check", "--server", name], "select 1;");
        assert_eq!(
            stderr(&output),
            format!(
                "kilit: no lab results for {name}; kilit has results for mysql-8.0.46, \
                 mysql-8.4.13\n"
            ),
            "{name:?}"
        );
        assert_eq!(stdout(&output), "", "{name:?}");
        assert_eq!(output.status.code(), Some(2), "{name:?}");
    }
}

#[test]
fn servers_marks_the_default_series() {
    let output = kilit(&["servers"], "");
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].starts_with("mysql-8.0.46  "), "{text}");
    assert!(lines[0].ends_with(" scenarios"), "{text}");
    assert!(!lines[0].contains("default"), "{text}");
    assert!(lines[1].starts_with("mysql-8.4.13  "), "{text}");
    assert!(
        lines[1].ends_with(" scenarios (default: mysql-8.4)"),
        "{text}"
    );
    assert_eq!(output.status.code(), Some(0));
}

// The other measured server.

/// The tables of the other-server example: `users` has a plain index on
/// `name`, and `orders` a column of the same type and collation.
const PLAIN_INDEX_TABLES: &str = "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(50) NOT NULL,
  PRIMARY KEY (`id`),
  KEY `users_name_index` (`name`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_name` varchar(50) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
";

/// A foreign key to a column with a plain index: 8.4.13 refuses it with
/// 6125, 8.0.46 takes it and copies the table. Then the column goes.
const FOREIGN_KEY_THEN_DROP: &str = "ALTER TABLE orders ADD CONSTRAINT orders_user_name_foreign \
FOREIGN KEY (user_name) REFERENCES users (name);
ALTER TABLE orders DROP COLUMN user_name;";

/// Writes `text` to a file of its own for the running test: tests run in
/// parallel, and each reads its schema while the others write theirs.
fn write_schema(name: &str, text: &str) -> PathBuf {
    let thread = format!("{:?}", std::thread::current().id()).replace(['(', ')'], "");
    let file = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{thread}-{name}"));
    std::fs::write(&file, text).unwrap();
    file
}

fn on_plain_index_tables(args: &[&str], input: &str) -> Output {
    let schema = write_schema("step12_plain_index_tables.sql", PLAIN_INDEX_TABLES);
    let mut all = vec!["check", "--schema", schema.to_str().unwrap()];
    all.extend(args);
    kilit(&all, input)
}

#[test]
fn the_other_server_is_named_in_a_last_note_when_it_answers_differently() {
    // $table->foreign('user_name')->references('name')->on('users'), then
    // $table->dropColumn('user_name'), after $table->index('name') on users.
    let text = stdout(&on_plain_index_tables(&[], FOREIGN_KEY_THEN_DROP));
    assert!(
        text.starts_with("checked against mysql-8.4.13, as measured by kilit-lab\n"),
        "{text}"
    );
    // 8.4.13 refuses the key; 8.0.46 copies the table.
    assert!(
        text.contains(
            "    fails with error 6125\n\
             \x20   measured as add-foreign-key ("
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "    - on mysql-8.0.46: COPY, copies every row, writes wait\n  line 2: ALTER TABLE orders DROP COLUMN user_name\n"
        ),
        "{text}"
    );
    // Because the key does not exist on 8.4.13, the column goes at once; on
    // 8.0.46 a foreign key uses it.
    assert!(
        text.contains(
            "  line 2: ALTER TABLE orders DROP COLUMN user_name\n\
             \x20   INSTANT, no rebuild, writes continue\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("    - on mysql-8.0.46: fails with error 1828\n\nmetadata lock on orders:"),
        "{text}"
    );
    assert!(
        text.ends_with("\n2 statements: 1 fails, 1 runs while writes continue\n"),
        "{text}"
    );
}

#[test]
fn the_same_input_for_the_other_server_shows_the_other_answers_and_names_the_first() {
    let text = stdout(&on_plain_index_tables(
        &["--server", "mysql-8.0"],
        FOREIGN_KEY_THEN_DROP,
    ));
    assert!(
        text.starts_with("checked against mysql-8.0.46, as measured by kilit-lab\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "  line 1: ALTER TABLE orders ADD CONSTRAINT orders_user_name_foreign FOREIGN KEY \
             (user_name) REFERENCES users (name)\n    COPY, copies every row, writes wait\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "    - on mysql-8.4.13: fails with error 6125\n  line 2: ALTER TABLE orders DROP COLUMN user_name\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "  line 2: ALTER TABLE orders DROP COLUMN user_name\n    fails with error 1828\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "    - on mysql-8.4.13: INSTANT, no rebuild, writes continue\n\nmetadata lock on orders:"
        ),
        "{text}"
    );
    assert!(
        text.ends_with("\n2 statements: 1 fails, 1 makes writes wait\n"),
        "{text}"
    );
    // The full name gives the same.
    let named = stdout(&on_plain_index_tables(
        &["--server", "mysql-8.0.46"],
        FOREIGN_KEY_THEN_DROP,
    ));
    assert_eq!(named, text);
}

#[test]
fn the_json_lists_the_other_server_on_each_statement_that_differs() {
    let json = json_of(&on_plain_index_tables(
        &["--format", "json"],
        FOREIGN_KEY_THEN_DROP,
    ));
    assert_eq!(json["server"], "mysql-8.4.13");
    let statements = statements_of(&json, 0);
    assert_eq!(statements[0]["impact"], "fails");
    assert_eq!(statements[0]["error"], 6125);
    assert_eq!(
        statements[0]["other_servers"],
        serde_json::json!([{
            "server": "mysql-8.0.46",
            "impact": "writes_wait",
            "answer": "COPY, copies every row, writes wait",
        }])
    );
    assert_eq!(statements[1]["impact"], "writes_continue");
    assert_eq!(statements[1]["algorithm"], "INSTANT");
    assert_eq!(
        statements[1]["other_servers"],
        serde_json::json!([{
            "server": "mysql-8.0.46",
            "impact": "fails",
            "answer": "fails with error 1828",
        }])
    );
    assert_eq!(json["summary"]["fails"], 1);
    assert_eq!(json["summary"]["writes_continue"], 1);

    let json = json_of(&on_plain_index_tables(
        &["--format", "json", "--server", "mysql-8.0"],
        FOREIGN_KEY_THEN_DROP,
    ));
    assert_eq!(json["server"], "mysql-8.0.46");
    let statements = statements_of(&json, 0);
    assert_eq!(statements[0]["impact"], "writes_wait");
    assert_eq!(statements[0]["algorithm"], "COPY");
    assert_eq!(statements[0]["copies_rows"], true);
    assert_eq!(
        statements[0]["other_servers"],
        serde_json::json!([{
            "server": "mysql-8.4.13",
            "impact": "fails",
            "answer": "fails with error 6125",
        }])
    );
    assert_eq!(statements[1]["impact"], "fails");
    assert_eq!(statements[1]["error"], 1828);
    assert_eq!(
        statements[1]["other_servers"],
        serde_json::json!([{
            "server": "mysql-8.4.13",
            "impact": "writes_continue",
            "answer": "INSTANT, no rebuild, writes continue",
        }])
    );
    assert_eq!(json["summary"]["fails"], 1);
    assert_eq!(json["summary"]["writes_wait"], 1);
}

#[test]
fn the_second_statement_differs_only_because_the_two_timelines_diverged() {
    // Alone, DROP COLUMN user_name is the same on both servers: no key uses
    // the column, so there is nothing to tell.
    let alone = "ALTER TABLE orders DROP COLUMN user_name;";
    for server in ["mysql-8.0", "mysql-8.4"] {
        let output = on_plain_index_tables(&["--format", "json", "--server", server], alone);
        let json = json_of(&output);
        let statement = &statements_of(&json, 0)[0];
        assert_eq!(statement["impact"], "writes_continue", "{server}");
        assert_eq!(statement["algorithm"], "INSTANT", "{server}");
        assert!(
            statement.get("other_servers").is_none(),
            "{server}: {statement}"
        );
        let text = stdout(&on_plain_index_tables(&["--server", server], alone));
        assert!(!text.contains("\n    - on mysql-"), "{server}: {text}");
    }
    // After the key, 8.0.46 has it and 8.4.13 does not.
    let json = json_of(&on_plain_index_tables(
        &["--format", "json", "--server", "mysql-8.0"],
        FOREIGN_KEY_THEN_DROP,
    ));
    assert_eq!(statements_of(&json, 0)[1]["error"], 1828);
    let json = json_of(&on_plain_index_tables(
        &["--format", "json", "--server", "mysql-8.4"],
        FOREIGN_KEY_THEN_DROP,
    ));
    assert_eq!(statements_of(&json, 0)[1]["algorithm"], "INSTANT");
}

#[test]
fn the_exit_status_follows_the_chosen_server_only() {
    let status = |args: &[&str]| {
        on_plain_index_tables(args, FOREIGN_KEY_THEN_DROP)
            .status
            .code()
    };
    // 8.4.13 fails the key (6125); 8.0.46 fails the DROP COLUMN (1828).
    assert_eq!(status(&[]), Some(1));
    assert_eq!(status(&["--server", "mysql-8.0"]), Some(1));
    // Only waits count: 8.4.13 has none, although 8.0.46 would make writes
    // wait for the key.
    assert_eq!(status(&["--fail-on", "waits"]), Some(0));
    assert_eq!(
        status(&["--server", "mysql-8.4", "--fail-on", "waits"]),
        Some(0)
    );
    assert_eq!(
        status(&["--server", "mysql-8.0", "--fail-on", "waits"]),
        Some(1)
    );
    // Only failures count: both servers fail one statement.
    assert_eq!(status(&["--fail-on", "fails"]), Some(1));
    assert_eq!(
        status(&["--server", "mysql-8.0", "--fail-on", "fails"]),
        Some(1)
    );
    // The JSON agrees with the process.
    let json = json_of(&on_plain_index_tables(
        &["--format", "json", "--fail-on", "waits"],
        FOREIGN_KEY_THEN_DROP,
    ));
    assert_eq!(json["exit_code"], 0);
    assert_eq!(json["summary"]["fails"], 1);
    let json = json_of(&on_plain_index_tables(
        &[
            "--format",
            "json",
            "--fail-on",
            "waits",
            "--server",
            "mysql-8.0",
        ],
        FOREIGN_KEY_THEN_DROP,
    ));
    assert_eq!(json["exit_code"], 1);
}

#[test]
fn a_run_where_the_servers_agree_has_no_other_servers_and_no_note() {
    let sample = example("sample.sql");
    let schema = example("mysql-schema.sql");
    let pretend = std::fs::read_to_string(example("laravel-pretend.txt")).unwrap();
    let sample_text = std::fs::read_to_string(&sample).unwrap();
    for (args, input) in [
        (vec!["--schema", schema.to_str().unwrap()], pretend.clone()),
        (vec![], pretend),
        (
            vec!["--schema", schema.to_str().unwrap()],
            sample_text.clone(),
        ),
        (vec![], sample_text),
        (vec![], RUNS.to_owned()),
        (vec![], FAILS.to_owned()),
    ] {
        for server in [None, Some("mysql-8.0")] {
            let mut all = vec!["check"];
            all.extend(&args);
            if let Some(server) = server {
                all.extend(["--server", server]);
            }
            let text = stdout(&kilit(&all, &input));
            assert!(!text.contains("\n    - on mysql-"), "{args:?} {server:?}");

            all.extend(["--format", "json"]);
            let output = kilit(&all, &input);
            let json = json_of(&output);
            assert!(
                !stdout(&output).contains("other_servers"),
                "{args:?} {server:?}"
            );
            for source in json["sources"].as_array().unwrap() {
                for statement in source["statements"].as_array().unwrap() {
                    assert!(
                        statement.get("other_servers").is_none(),
                        "{args:?} {server:?}: {statement}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_statement_that_fails_the_same_way_on_both_servers_has_no_note() {
    // 3780 and 1674 are the same on both servers.
    let sql = format!("{FAILS}\nalter table `orders` add `n` int not null;");
    let output = on_example_tables(&["--format", "json"], &sql);
    assert!(!stdout(&output).contains("other_servers"));
    let text = stdout(&on_example_tables(&[], &sql));
    assert!(!text.contains("\n    - on mysql-"), "{text}");
}

// The server version a schema dump names (mysqldump writes
// `-- Server version<TAB>X` near the top; its `Distrib` line is the client's).

/// The first lines of `mysqldump --no-data` 8.0.46 on an 8.0.46 server.
const DUMP_8_0_ON_8_0: &str = "-- MySQL dump 10.13  Distrib 8.0.46, for Linux (x86_64)
--
-- Host: localhost    Database: kilit_dump12
-- ------------------------------------------------------
-- Server version\t8.0.46

/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;
/*!50503 SET NAMES utf8mb4 */;
";
/// The same for 8.4.13.
const DUMP_8_4_ON_8_4: &str = "-- MySQL dump 10.13  Distrib 8.4.13, for Linux (x86_64)
--
-- Host: localhost    Database: kilit_dump12
-- ------------------------------------------------------
-- Server version\t8.4.13

/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;
/*!50503 SET NAMES utf8mb4 */;
";
/// The 8.4.13 client against the 8.0.46 server.
const DUMP_8_4_CLIENT_ON_8_0_SERVER: &str =
    "-- MySQL dump 10.13  Distrib 8.4.13, for Linux (x86_64)
--
-- Host: mysql80    Database: kilit_dump12
-- ------------------------------------------------------
-- Server version\t8.0.46

/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;
/*!50503 SET NAMES utf8mb4 */;
";
/// The 8.0.46 client against the 8.4.13 server.
const DUMP_8_0_CLIENT_ON_8_4_SERVER: &str =
    "-- MySQL dump 10.13  Distrib 8.0.46, for Linux (x86_64)
--
-- Host: mysql84    Database: kilit_dump12
-- ------------------------------------------------------
-- Server version\t8.4.13

/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;
/*!50503 SET NAMES utf8mb4 */;
";
/// What Laravel's schema:dump writes (--skip-comments): no header at all.
const LARAVEL_DUMP_HEADER: &str = "/*!40103 SET @OLD_TIME_ZONE=@@TIME_ZONE */;
/*!40103 SET TIME_ZONE='+00:00' */;
/*!40014 SET @OLD_UNIQUE_CHECKS=@@UNIQUE_CHECKS, UNIQUE_CHECKS=0 */;
";
/// mariadb-dump of a 10.11 server.
const MARIADB_HEADER: &str =
    "-- MariaDB dump 10.19  Distrib 10.11.6-MariaDB, for debian-linux-gnu (x86_64)
--
-- Host: localhost    Database: app
-- ------------------------------------------------------
-- Server version\t10.11.6-MariaDB-0ubuntu0.23.04.1

";
/// The line newer mariadb-dump versions write before the banner.
const MARIADB_SANDBOX: &str = "/*M!999999\\- enable the sandbox mode */ \n";
const MARIADB_NAME: &str = "10.11.6-MariaDB-0ubuntu0.23.04.1";

const DUMP_TABLES: &str = "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(50) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;

CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `amount` int NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
";

/// kilit over `input`, with a schema made of `header` and two tables.
fn with_dump_header(header: &str, args: &[&str], input: &str) -> Output {
    let schema = write_schema("step12_dump_header.sql", &format!("{header}{DUMP_TABLES}"));
    let mut all = vec!["check", "--schema", schema.to_str().unwrap()];
    all.extend(args);
    kilit(&all, input)
}

/// The warning lines of the text: the lines between "tables from" and the
/// first blank line.
fn text_warnings(text: &str) -> Vec<String> {
    let mut lines = text
        .lines()
        .skip_while(|line| !line.starts_with("tables from "));
    assert!(lines.next().is_some(), "{text}");
    lines
        .take_while(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The messages of the `server-mismatch` warnings of the JSON, in order.
fn mismatch_messages(json: &serde_json::Value) -> Vec<String> {
    json["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning["id"] == "server-mismatch")
        .map(|warning| warning["message"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_dump_from_another_series_than_the_checked_one_gets_a_warning_right_after_the_tables_line() {
    let output = with_dump_header(DUMP_8_0_ON_8_0, &[], RUNS);
    let text = stdout(&output);
    assert_eq!(
        text_warnings(&text),
        ["  the schema comes from MySQL 8.0.46; answers are for mysql-8.4.13"],
        "{text}"
    );
    assert_eq!(output.status.code(), Some(0), "{text}");

    let json = json_of(&with_dump_header(
        DUMP_8_0_ON_8_0,
        &["--format", "json"],
        RUNS,
    ));
    assert!(
        json["warnings"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!({
                "id": "server-mismatch",
                "tables": [],
                "message": "the schema comes from MySQL 8.0.46; answers are for mysql-8.4.13",
            })),
        "{json}"
    );
}

#[test]
fn a_dump_of_the_checked_series_gets_no_warning() {
    for (header, args) in [
        (DUMP_8_4_ON_8_4, vec![]),
        (DUMP_8_4_ON_8_4, vec!["--server", "mysql-8.4"]),
        (DUMP_8_4_ON_8_4, vec!["--server", "mysql-8.4.13"]),
        (DUMP_8_0_ON_8_0, vec!["--server", "mysql-8.0"]),
        (DUMP_8_0_ON_8_0, vec!["--server", "mysql-8.0.46"]),
    ] {
        let output = with_dump_header(header, &args, RUNS);
        let text = stdout(&output);
        assert_eq!(
            text_warnings(&text),
            Vec::<String>::new(),
            "{args:?}: {text}"
        );
        assert!(!text.contains("the schema comes from"), "{args:?}: {text}");
        let mut json_args = vec!["--format", "json"];
        json_args.extend(&args);
        let json = json_of(&with_dump_header(header, &json_args, RUNS));
        assert_eq!(mismatch_messages(&json), Vec::<String>::new(), "{args:?}");
    }
}

#[test]
fn a_dump_of_the_other_series_warns_whichever_server_is_chosen() {
    for (header, server, message) in [
        (
            DUMP_8_4_ON_8_4,
            "mysql-8.0",
            "the schema comes from MySQL 8.4.13; answers are for mysql-8.0.46",
        ),
        (
            DUMP_8_0_ON_8_0,
            "mysql-8.4",
            "the schema comes from MySQL 8.0.46; answers are for mysql-8.4.13",
        ),
        (
            DUMP_8_0_ON_8_0,
            "mysql-8.4.13",
            "the schema comes from MySQL 8.0.46; answers are for mysql-8.4.13",
        ),
    ] {
        let text = stdout(&with_dump_header(header, &["--server", server], RUNS));
        assert_eq!(
            text_warnings(&text),
            [format!("  {message}")],
            "{server}: {text}"
        );
        let json = json_of(&with_dump_header(
            header,
            &["--server", server, "--format", "json"],
            RUNS,
        ));
        assert_eq!(mismatch_messages(&json), [message], "{server}");
    }
}

#[test]
fn only_the_server_version_line_of_a_dump_is_read_not_the_client_s_distrib_line() {
    // mysqldump 8.4.13 against an 8.0.46 server: the server is 8.0.
    let text = stdout(&with_dump_header(DUMP_8_4_CLIENT_ON_8_0_SERVER, &[], RUNS));
    assert_eq!(
        text_warnings(&text),
        ["  the schema comes from MySQL 8.0.46; answers are for mysql-8.4.13"],
        "{text}"
    );
    let text = stdout(&with_dump_header(
        DUMP_8_4_CLIENT_ON_8_0_SERVER,
        &["--server", "mysql-8.0"],
        RUNS,
    ));
    assert_eq!(text_warnings(&text), Vec::<String>::new(), "{text}");

    // mysqldump 8.0.46 against an 8.4.13 server: the server is 8.4.
    let text = stdout(&with_dump_header(DUMP_8_0_CLIENT_ON_8_4_SERVER, &[], RUNS));
    assert_eq!(text_warnings(&text), Vec::<String>::new(), "{text}");
    let text = stdout(&with_dump_header(
        DUMP_8_0_CLIENT_ON_8_4_SERVER,
        &["--server", "mysql-8.0"],
        RUNS,
    ));
    assert_eq!(
        text_warnings(&text),
        ["  the schema comes from MySQL 8.4.13; answers are for mysql-8.0.46"],
        "{text}"
    );
}

#[test]
fn a_dump_without_a_server_version_line_gets_no_warning() {
    for (what, header) in [
        // Only the client's version.
        (
            "distrib only",
            "-- MySQL dump 10.13  Distrib 8.0.46, for Linux (x86_64)\n--\n\n",
        ),
        // What Laravel's schema:dump leaves.
        ("laravel dump", LARAVEL_DUMP_HEADER),
        ("no header", ""),
        // Not the line mysqldump writes.
        ("other text", "-- the server version\t8.0.46\n"),
    ] {
        let output = with_dump_header(header, &[], RUNS);
        let text = stdout(&output);
        assert_eq!(text_warnings(&text), Vec::<String>::new(), "{what}: {text}");
        assert_eq!(output.status.code(), Some(0), "{what}: {text}");
        let json = json_of(&with_dump_header(header, &["--format", "json"], RUNS));
        assert_eq!(mismatch_messages(&json), Vec::<String>::new(), "{what}");
    }
}

#[test]
fn a_mariadb_dump_without_server_stops_with_2_and_says_what_to_do() {
    for header in [
        MARIADB_HEADER.to_owned(),
        format!("{MARIADB_SANDBOX}{MARIADB_HEADER}"),
    ] {
        let output = with_dump_header(&header, &[], RUNS);
        assert_eq!(
            stderr(&output),
            format!(
                "kilit: the schema comes from MariaDB {MARIADB_NAME}, and kilit has no MariaDB \
                 measurements; give --server to check it against MySQL anyway\n"
            ),
            "{header}"
        );
        assert_eq!(stdout(&output), "", "{header}");
        assert_eq!(output.status.code(), Some(2), "{header}");
        let output = with_dump_header(&header, &["--format", "json"], RUNS);
        assert_eq!(stdout(&output), "", "{header}");
        assert_eq!(output.status.code(), Some(2), "{header}");
    }
}

#[test]
fn a_mariadb_dump_with_server_runs_with_a_warning() {
    for header in [
        MARIADB_HEADER.to_owned(),
        format!("{MARIADB_SANDBOX}{MARIADB_HEADER}"),
    ] {
        for (server, release) in [
            ("mysql-8.4", "mysql-8.4.13"),
            ("mysql-8.4.13", "mysql-8.4.13"),
            ("mysql-8.0", "mysql-8.0.46"),
        ] {
            let message = format!(
                "the schema comes from MariaDB {MARIADB_NAME}, and kilit has no MariaDB \
                 measurements; answers are for {release}"
            );
            let output = with_dump_header(&header, &["--server", server], RUNS);
            let text = stdout(&output);
            assert_eq!(
                text_warnings(&text),
                [format!("  {message}")],
                "{server}: {text}"
            );
            assert_eq!(output.status.code(), Some(0), "{server}: {text}");
            assert_eq!(stderr(&output), "", "{server}");
            let json = json_of(&with_dump_header(
                &header,
                &["--server", server, "--format", "json"],
                RUNS,
            ));
            assert!(
                json["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|warning| warning["message"] == message.as_str()),
                "{server}: {json}"
            );
        }
    }
}

#[test]
fn two_server_version_lines_of_two_versions_give_two_warnings_in_order() {
    let header = "-- Server version\t8.0.46\n\n-- Server version\t5.7.44\n";
    let text = stdout(&with_dump_header(header, &[], RUNS));
    assert_eq!(
        text_warnings(&text),
        [
            "  the schema comes from MySQL 8.0.46; answers are for mysql-8.4.13",
            "  the schema comes from MySQL 5.7.44; answers are for mysql-8.4.13",
        ],
        "{text}"
    );
    let json = json_of(&with_dump_header(header, &["--format", "json"], RUNS));
    assert_eq!(
        mismatch_messages(&json),
        [
            "the schema comes from MySQL 8.0.46; answers are for mysql-8.4.13",
            "the schema comes from MySQL 5.7.44; answers are for mysql-8.4.13",
        ]
    );
    // A joined dump of the checked series and another: one warning.
    let header = "-- Server version\t8.4.13\n\n-- Server version\t8.0.46\n";
    let text = stdout(&with_dump_header(header, &[], RUNS));
    assert_eq!(
        text_warnings(&text),
        ["  the schema comes from MySQL 8.0.46; answers are for mysql-8.4.13"],
        "{text}"
    );
}

#[test]
fn the_same_server_version_named_twice_gives_one_warning() {
    let header = "-- Server version\t8.0.46\n\n-- Server version\t8.0.46\n";
    let text = stdout(&with_dump_header(header, &[], RUNS));
    assert_eq!(
        text_warnings(&text),
        ["  the schema comes from MySQL 8.0.46; answers are for mysql-8.4.13"],
        "{text}"
    );
    let json = json_of(&with_dump_header(header, &["--format", "json"], RUNS));
    assert_eq!(mismatch_messages(&json).len(), 1);
}

#[test]
fn a_server_version_kilit_cannot_read_gets_a_warning_that_names_it() {
    let text = stdout(&with_dump_header("-- Server version\tweird\n", &[], RUNS));
    assert_eq!(
        text_warnings(&text),
        [
            "  the schema names its server version as \"weird\", which kilit cannot read; \
             answers are for mysql-8.4.13"
        ],
        "{text}"
    );
    let json = json_of(&with_dump_header(
        "-- Server version\tweird\n",
        &["--format", "json", "--server", "mysql-8.0"],
        RUNS,
    ));
    assert_eq!(
        mismatch_messages(&json),
        [
            "the schema names its server version as \"weird\", which kilit cannot read; \
             answers are for mysql-8.0.46"
        ]
    );
}

#[test]
fn a_server_version_line_in_a_migration_file_is_not_read() {
    let schema = example("mysql-schema.sql");
    let schema = schema.to_str().unwrap();
    for line in [
        "-- Server version\t8.0.46",
        "-- Server version\t10.11.6-MariaDB-0ubuntu0.23.04.1",
        "-- Server version\tweird",
    ] {
        let sql = format!("{line}\n{RUNS}\n");
        for args in [vec!["check"], vec!["check", "--schema", schema]] {
            let output = kilit(&args, &sql);
            let text = stdout(&output);
            assert!(!text.contains("the schema comes from"), "{line}: {text}");
            assert!(!text.contains("the schema names"), "{line}: {text}");
            assert_eq!(output.status.code(), Some(0), "{line}: {text}");
            assert_eq!(stderr(&output), "", "{line}");
        }
        let mut json_args = vec!["check", "--format", "json"];
        json_args.extend(["--schema", schema]);
        let json = json_of(&kilit(&json_args, &sql));
        assert_eq!(mismatch_messages(&json), Vec::<String>::new(), "{line}");
    }
}

#[test]
fn the_server_warning_does_not_change_the_exit_status() {
    for (sql, code) in [(RUNS, 0), (FAILS, 1), (WAITS, 1), (UNKNOWN, 0)] {
        for header in [DUMP_8_0_ON_8_0, DUMP_8_4_ON_8_4] {
            let output = with_dump_header(header, &[], sql);
            assert_eq!(output.status.code(), Some(code), "{sql}");
            let output = with_dump_header(header, &["--strict"], sql);
            let strict = if sql == UNKNOWN { 3 } else { code };
            assert_eq!(output.status.code(), Some(strict), "{sql}");
        }
    }
    let json = json_of(&with_dump_header(
        DUMP_8_0_ON_8_0,
        &["--format", "json"],
        FAILS,
    ));
    assert_eq!(json["exit_code"], 1);
}

// The file a Laravel error belongs to.

fn laravel_error_warnings(json: &serde_json::Value) -> Vec<serde_json::Value> {
    json["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning["id"] == "laravel-error")
        .cloned()
        .collect()
}

#[test]
fn a_laravel_error_read_from_standard_input_names_standard_input_in_the_json_only() {
    let output = kilit(&["check", "--format", "json"], C1_THROW_L12);
    let json = json_of(&output);
    let warnings = laravel_error_warnings(&json);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["file"], "standard input");
    assert_eq!(warnings[0]["message"], stopped_in(THROW_NAME));
    assert_eq!(output.status.code(), Some(2));

    // One input: the text line has no file in front of it.
    let text = stdout(&kilit(&["check"], C1_THROW_L12));
    assert!(
        text.contains(&format!("\n\n{}\n\n", stopped_in(THROW_NAME))),
        "{text}"
    );
    assert!(!text.contains("standard input: Laravel"), "{text}");
}

#[test]
fn a_laravel_error_read_from_one_file_names_the_file_in_the_json_only() {
    let files = session_files("step12_error_one_file", &[("error.txt", C1_THROW_L12)]);
    let path = files[0].to_str().unwrap();
    let output = kilit(&["check", "--format", "json", path], "");
    let json = json_of(&output);
    let warnings = laravel_error_warnings(&json);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["file"], path);
    assert_eq!(warnings[0]["message"], stopped_in(THROW_NAME));
    assert_eq!(output.status.code(), Some(2));

    let text = stdout(&kilit(&["check", path], ""));
    assert!(
        text.contains(&format!("\n\n{}\n\n", stopped_in(THROW_NAME))),
        "{text}"
    );
    assert!(!text.contains(&format!("{path}: Laravel")), "{text}");
}

#[test]
fn a_laravel_error_among_several_files_names_its_file_in_the_text_and_the_json() {
    let files = session_files(
        "step12_error_two_files",
        &[("alter.sql", RUNS), ("error.txt", C1_THROW_L12)],
    );
    let alter = files[0].to_str().unwrap();
    let error = files[1].to_str().unwrap();

    let output = kilit(&["check", alter, error], "");
    let text = stdout(&output);
    assert!(
        text.contains(&format!("\n\n{error}: {}\n\n", stopped_in(THROW_NAME))),
        "{text}"
    );
    assert!(!text.contains(&format!("{alter}: Laravel")), "{text}");
    assert_eq!(output.status.code(), Some(2), "{text}");

    let output = kilit(&["check", "--format", "json", alter, error], "");
    let warnings = laravel_error_warnings(&json_of(&output));
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0]["file"], error);
    assert_eq!(warnings[0]["message"], stopped_in(THROW_NAME));
    assert_eq!(output.status.code(), Some(2));

    // The error file first: the same file is named.
    let output = kilit(&["check", "--format", "json", error, alter], "");
    let warnings = laravel_error_warnings(&json_of(&output));
    assert_eq!(warnings[0]["file"], error);
}

#[test]
fn a_laravel_error_on_standard_input_among_files_is_named_standard_input() {
    let files = session_files("step12_error_stdin_and_file", &[("alter.sql", RUNS)]);
    let alter = files[0].to_str().unwrap();
    let text = stdout(&kilit(&["check", alter, "-"], C1_THROW_L12));
    assert!(
        text.contains(&format!(
            "\n\nstandard input: {}\n\n",
            stopped_in(THROW_NAME)
        )),
        "{text}"
    );
    let json = json_of(&kilit(
        &["check", "--format", "json", alter, "-"],
        C1_THROW_L12,
    ));
    let warnings = laravel_error_warnings(&json);
    assert_eq!(warnings[0]["file"], "standard input");
}

// The other server's note on a table the input creates.

/// A child table created here, then a foreign key from it to `users.name`,
/// which has a plain index. 8.4.13 refuses the key with 6125; 8.0.46 copies
/// the table, which costs nothing on a table created in the same input.
const NEW_CHILD_FOREIGN_KEY: &str = "CREATE TABLE child (id bigint unsigned NOT NULL PRIMARY KEY, \
user_name varchar(50) NOT NULL) DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
ALTER TABLE child ADD CONSTRAINT child_user_name_foreign \
FOREIGN KEY (user_name) REFERENCES users (name);";

/// The lines of the statement that starts with `line {number}:`, up to the
/// blank line after it.
fn statement_block(text: &str, number: usize) -> Vec<&str> {
    let start = format!("  line {number}:");
    text.lines()
        .skip_while(|line| !line.starts_with(&start))
        .take_while(|line| !line.is_empty())
        .collect()
}

#[test]
fn the_other_server_s_answer_on_a_table_created_here_says_that_nothing_waits_for_it() {
    // Schema::create('child', ...) then
    // $table->foreign('user_name')->references('name')->on('users').
    const COPY_NOTE: &str = "COPY, copies every row, writes wait; child is created earlier in \
these migrations, so nothing waits for it";

    // Default server (8.4.13): the key fails, and 8.0.46 would copy the table,
    // but the table is new there, so the note says nothing waits.
    let output = on_plain_index_tables(&[], NEW_CHILD_FOREIGN_KEY);
    let text = stdout(&output);
    let block = statement_block(&text, 2);
    assert_eq!(
        block.last().copied(),
        Some(format!("    - on mysql-8.0.46: {COPY_NOTE}").as_str()),
        "{text}"
    );
    assert!(block.contains(&"    fails with error 6125"), "{text}");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));

    let json = json_of(&on_plain_index_tables(
        &["--format", "json"],
        NEW_CHILD_FOREIGN_KEY,
    ));
    let statements = statements_of(&json, 0);
    assert_eq!(statements[1]["impact"], "fails");
    assert_eq!(statements[1]["error"], 6125);
    assert_eq!(
        statements[1]["other_servers"],
        serde_json::json!([{
            "server": "mysql-8.0.46",
            "impact": "new_table",
            "answer": COPY_NOTE,
        }])
    );

    // --server mysql-8.0: the statement is a new-table one here, with its own
    // note, and 8.4.13 is the other server that fails.
    let output = on_plain_index_tables(&["--server", "mysql-8.0"], NEW_CHILD_FOREIGN_KEY);
    let text = stdout(&output);
    assert!(
        text.starts_with("checked against mysql-8.0.46, as measured by kilit-lab\n"),
        "{text}"
    );
    let block = statement_block(&text, 2);
    assert_eq!(
        block.last().copied(),
        Some("    - on mysql-8.4.13: fails with error 6125"),
        "{text}"
    );
    assert!(
        block.contains(
            &"    - child is created earlier in these migrations: nothing else uses it yet, \
so nothing waits for it"
        ),
        "{text}"
    );

    let json = json_of(&on_plain_index_tables(
        &["--format", "json", "--server", "mysql-8.0"],
        NEW_CHILD_FOREIGN_KEY,
    ));
    let statements = statements_of(&json, 0);
    assert_eq!(statements[1]["impact"], "new_table");
    assert_eq!(
        statements[1]["other_servers"],
        serde_json::json!([{
            "server": "mysql-8.4.13",
            "impact": "fails",
            "answer": "fails with error 6125",
        }])
    );
}

// The documentation is guarded like the README example: what it shows must
// be what kilit does, and what it links to must exist.

/// The repository's root, where README.md and docs/ live.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// What docs/json-output.md asks to run, found in the document itself.
struct ShownCommand {
    /// The directory of the nearest `$ cd` line before the command, relative
    /// to the repository root.
    directory: String,
    /// The words between `kilit` and `<<'SQL'`, `check` among them.
    args: Vec<String>,
    /// The lines of the here-document, up to the line `SQL`.
    stdin: String,
    /// The complete output the document shows: the first ```json block
    /// after the command.
    output: String,
}

/// Every `$ kilit check ` command of the document, each with its own
/// directory and its own output block.
fn shown_commands(document: &str) -> Vec<ShownCommand> {
    let lines: Vec<&str> = document.lines().collect();
    let commands: Vec<usize> = (0..lines.len())
        .filter(|at| lines[*at].starts_with("$ kilit check "))
        .collect();
    assert!(
        !commands.is_empty(),
        "docs/json-output.md shows a line beginning `$ kilit check `"
    );

    let mut shown = Vec::new();
    for (number, &command) in commands.iter().enumerate() {
        let line = lines[command];
        let directory = lines[..command]
            .iter()
            .rev()
            .find_map(|earlier| earlier.strip_prefix("$ cd "))
            .unwrap_or_else(|| panic!("a `$ cd <directory>` line comes before the command: {line}"))
            .trim()
            .to_string();

        assert!(
            line.contains("<<'SQL'"),
            "the command feeds its input with <<'SQL': {line}"
        );
        let words = line
            .strip_prefix("$ kilit ")
            .unwrap()
            .split("<<'SQL'")
            .next()
            .unwrap();
        let args = words.split_whitespace().map(str::to_string).collect();

        // The command's own block lies before the next command.
        let limit = commands.get(number + 1).copied().unwrap_or(lines.len());
        let body = &lines[command + 1..limit];
        let end = body
            .iter()
            .position(|line| *line == "SQL")
            .unwrap_or_else(|| panic!("the here-document of `{line}` ends with a line SQL"));
        let mut stdin = body[..end].join("\n");
        stdin.push('\n');

        let after = &body[end + 1..];
        let start = after
            .iter()
            .position(|line| line.trim_end() == "```json")
            .unwrap_or_else(|| panic!("a ```json block follows the command `{line}`"));
        let length = after[start + 1..]
            .iter()
            .position(|line| line.trim_end() == "```")
            .unwrap_or_else(|| panic!("the ```json block of `{line}` is closed"));
        let output = after[start + 1..start + 1 + length].join("\n");

        shown.push(ShownCommand {
            directory,
            args,
            stdin,
            output,
        });
    }
    shown
}

#[test]
fn the_json_example_is_real_output() {
    // The document says what to type and what comes out. The test types
    // every such example: the arguments, the input and the directory are
    // all read from the document, so a changed example has to be real as
    // well.
    let path = repository_root().join("docs/json-output.md");
    let document = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));

    for shown in shown_commands(&document) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_kilit"))
            .args(&shown.args)
            .current_dir(repository_root().join(&shown.directory))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(shown.stdin.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let real = stdout(&output);

        assert_eq!(
            shown.output.trim_end(),
            real.trim_end(),
            "docs/json-output.md shows a different output than kilit prints \
             for `kilit {}` in {} (stderr: {})",
            shown.args.join(" "),
            shown.directory,
            stderr(&output)
        );
    }
}

/// The lines of a Markdown text outside fenced code blocks. The lines that
/// open and close a fence are left out too.
fn prose_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let mark = trimmed.chars().next().filter(|c| *c == '`' || *c == '~');
        if let Some(mark) = mark {
            let length = trimmed.chars().take_while(|c| *c == mark).count();
            if length >= 3 {
                match fence {
                    None => fence = Some((mark, length)),
                    Some((open, size))
                        if open == mark
                            && length >= size
                            && trimmed.trim_matches(mark).trim().is_empty() =>
                    {
                        fence = None
                    }
                    Some(_) => {}
                }
                continue;
            }
        }
        if fence.is_none() {
            lines.push(line);
        }
    }
    lines
}

/// The line with every inline code span replaced by a space.
fn without_code_spans(line: &str) -> String {
    // A run of backticks closes at the next run of the same length.
    let chars: Vec<char> = line.chars().collect();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            text.push(chars[i]);
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] == '`' {
            i += 1;
        }
        let run = i - start;
        let mut j = i;
        let mut closed = None;
        while j < chars.len() {
            if chars[j] != '`' {
                j += 1;
                continue;
            }
            let from = j;
            while j < chars.len() && chars[j] == '`' {
                j += 1;
            }
            if j - from == run {
                closed = Some(j);
                break;
            }
        }
        match closed {
            Some(end) => {
                text.push(' ');
                i = end;
            }
            // No closing run: the backticks are plain text.
            None => text.extend(std::iter::repeat_n('`', run)),
        }
    }
    text
}

/// What stands between `<` and `>` on a line, for each HTML tag.
fn html_tags(line: &str) -> Vec<&str> {
    let mut tags = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        let after = &rest[open + 1..];
        let end = after.find('>').unwrap_or(after.len());
        tags.push(&after[..end]);
        rest = &after[end..];
    }
    tags
}

/// The quoted value of an attribute inside a tag, such as `href`.
fn html_attribute(tag: &str, name: &str) -> Option<String> {
    for (at, _) in tag.match_indices(name) {
        let before = tag[..at].chars().next_back();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let after = &tag[at + name.len()..];
        let Some(value) = after.strip_prefix('=') else {
            continue;
        };
        let Some(quote) = value.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        let value = &value[1..];
        if let Some(end) = value.find(quote) {
            return Some(value[..end].to_string());
        }
    }
    None
}

/// The targets of the links on one line: inline links, a reference-style
/// definition (`[label]: target`) and `href` / `src` in HTML. Inline code
/// spans are left out.
fn link_targets(line: &str) -> Vec<String> {
    let text = without_code_spans(line);

    let mut targets = Vec::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("](") {
        let inside = &rest[at + 2..];
        // The link ends at the parenthesis that balances the opening one.
        let mut depth = 1;
        let mut end = None;
        for (index, c) in inside.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        targets.push(inside[..end].to_string());
        rest = &inside[end..];
    }

    // A definition starts the line, indented by at most three spaces. A
    // footnote (`[^1]: ...`) is not a link.
    let trimmed = text.trim_start();
    if text.len() - trimmed.len() <= 3
        && let Some(inside) = trimmed.strip_prefix('[')
        && !inside.starts_with('^')
        && let Some((_, target)) = inside.split_once("]:")
        && !target.trim().is_empty()
    {
        targets.push(target.trim().to_string());
    }

    for tag in html_tags(&text) {
        for name in ["href", "src"] {
            targets.extend(html_attribute(tag, name));
        }
    }
    targets
}

/// A link target split into the file it names and the anchor after `#`, or
/// None when the link leaves the repository. The file is empty when the
/// link points inside the page.
fn split_link(target: &str) -> Option<(String, Option<String>)> {
    let target = target.trim();
    let target = match target.strip_prefix('<') {
        Some(inner) => inner.split('>').next().unwrap(),
        // A title follows the address after a space: [x](a.md "title").
        None => target.split_whitespace().next().unwrap_or(""),
    };
    if target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("mailto:")
    {
        return None;
    }
    let (file, anchor) = match target.split_once('#') {
        Some((file, anchor)) => (file, Some(anchor)),
        None => (target, None),
    };
    let anchor = anchor.filter(|anchor| !anchor.is_empty());
    if file.is_empty() && anchor.is_none() {
        return None;
    }
    Some((file.to_string(), anchor.map(str::to_string)))
}

/// The text of a heading line (`## Title`), or None for any other line.
fn heading_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    let rest = trimmed[level..].strip_prefix(char::is_whitespace)?;
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = rest.trim();
    // A closing run of # counts only after a space.
    let without_closing = rest.trim_end_matches('#');
    if without_closing.is_empty() {
        return Some("");
    }
    if without_closing.ends_with(char::is_whitespace) {
        Some(without_closing.trim_end())
    } else {
        Some(rest)
    }
}

/// The anchor GitHub gives a heading: link addresses and HTML tags are not
/// part of the text, letters go lower case, everything but letters, digits,
/// spaces, `-` and `_` is dropped, and spaces become `-`.
fn heading_slug(heading: &str) -> String {
    let chars: Vec<char> = heading.chars().collect();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '[' => {
                // [text](address) keeps the text only.
                let close = chars[i..].iter().position(|c| *c == ']').map(|at| i + at);
                let address_end = close
                    .filter(|close| chars.get(close + 1) == Some(&'('))
                    .and_then(|close| {
                        chars[close..]
                            .iter()
                            .position(|c| *c == ')')
                            .map(|at| close + at)
                    });
                if let (Some(close), Some(end)) = (close, address_end) {
                    text.extend(&chars[i + 1..close]);
                    i = end + 1;
                } else {
                    i += 1;
                }
            }
            '<' => match chars[i..].iter().position(|c| *c == '>') {
                Some(at) => i += at + 1,
                None => i += 1,
            },
            c => {
                text.push(c);
                i += 1;
            }
        }
    }
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// The anchors of a Markdown text: its headings (a repeated slug gets -1,
/// -2, ... in order; headings in fenced code do not count) and the
/// `<a id="...">` / `<a name="...">` anchors.
fn anchors_of(text: &str) -> HashSet<String> {
    let mut anchors = HashSet::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for line in prose_lines(text) {
        if let Some(heading) = heading_text(line) {
            let slug = heading_slug(heading);
            let times = seen.entry(slug.clone()).or_insert(0);
            anchors.insert(if *times == 0 {
                slug
            } else {
                format!("{slug}-{times}")
            });
            *times += 1;
        }
        for tag in html_tags(line) {
            let is_link = tag.starts_with('a') && tag[1..].starts_with(char::is_whitespace);
            if is_link {
                for name in ["id", "name"] {
                    anchors.extend(html_attribute(tag, name));
                }
            }
        }
    }
    anchors
}

/// Every link of a Markdown file that does not point at something that
/// exists, as `file: link`: a missing file, or an anchor that no heading of
/// a Markdown target has. Fenced code blocks are not read; anchors into
/// other kinds of files are not checked.
fn broken_links(
    root: &Path,
    file: &Path,
    anchors: &mut HashMap<PathBuf, HashSet<String>>,
) -> Vec<String> {
    let text = std::fs::read_to_string(file)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", file.display()));
    let name = file
        .strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string();
    let directory = file.parent().unwrap();

    let mut broken = Vec::new();
    for line in prose_lines(&text) {
        for target in link_targets(line) {
            let Some((path, anchor)) = split_link(&target) else {
                continue;
            };
            let linked = if path.is_empty() {
                file.to_path_buf()
            } else {
                directory.join(&path)
            };
            if !linked.exists() {
                broken.push(format!("{name}: {target}"));
                continue;
            }
            let Some(anchor) = anchor else { continue };
            if linked.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let known = anchors.entry(linked.clone()).or_insert_with(|| {
                let linked_text = std::fs::read_to_string(&linked)
                    .unwrap_or_else(|error| panic!("cannot read {}: {error}", linked.display()));
                anchors_of(&linked_text)
            });
            if !known.contains(&anchor) {
                broken.push(format!(
                    "{name}: {target} (no heading or anchor `{anchor}`)"
                ));
            }
        }
    }
    broken
}

#[test]
fn every_relative_link_points_at_a_file() {
    let root = repository_root();
    let mut files = vec![
        root.join("README.md"),
        root.join("CONTRIBUTING.md"),
        root.join("scenarios/README.md"),
    ];
    let mut docs: Vec<PathBuf> = std::fs::read_dir(root.join("docs"))
        .unwrap_or_else(|error| panic!("cannot read docs/: {error}"))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    assert!(!docs.is_empty(), "docs/ has no Markdown file");
    docs.sort();
    files.extend(docs);

    let mut anchors = HashMap::new();
    let mut broken = Vec::new();
    for file in &files {
        broken.extend(broken_links(&root, file, &mut anchors));
    }
    assert!(
        broken.is_empty(),
        "links that point at nothing:\n{}",
        broken.join("\n")
    );
}

// Step 12b: the metadata lock warning also names the tables a DROP TABLE,
// RENAME TABLE or TRUNCATE TABLE waits for. The lab measured on MySQL 8.0.46
// and 8.4.13 that each of them waits for an open transaction that read the
// table, that readers queue behind it, and that reads go on after error 1205;
// LOCK TABLES did not wait for a reader, so it stays out of the warning.

const LOCK_WARNING_STATEMENT_KINDS: &str =
    "Every ALTER TABLE, DROP TABLE, RENAME TABLE and TRUNCATE TABLE first waits";

fn write_lock_schema() -> PathBuf {
    let file = Path::new(env!("CARGO_TARGET_TMPDIR")).join("metadata-lock-schema.sql");
    std::fs::write(
        &file,
        "CREATE TABLE `users` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(100) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;

CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `amount` int NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;

CREATE TABLE `customers` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(100) NOT NULL,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci;
",
    )
    .unwrap();
    file
}

/// Text, JSON and exit status of `kilit check --schema <the lock schema>`
/// with `extra` arguments.
fn lock_run(extra: &[&str], input: &str) -> (String, serde_json::Value, Option<i32>) {
    let schema = write_lock_schema();
    let mut args = vec!["--schema", schema.to_str().unwrap()];
    args.extend(extra);
    text_and_json(&args, input)
}

/// The tables of the `metadata-lock` warning of the JSON, or None when it has
/// none.
fn locked_tables(json: &serde_json::Value) -> Option<Vec<String>> {
    let warnings = json["warnings"].as_array().unwrap();
    let locks: Vec<&serde_json::Value> = warnings
        .iter()
        .filter(|warning| warning["id"] == "metadata-lock")
        .collect();
    assert!(locks.len() <= 1, "{json}");
    locks.first().map(|warning| {
        warning["tables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|table| table.as_str().unwrap().to_owned())
            .collect()
    })
}

/// Asserts that the warning of `text` and `json` names exactly `tables`.
fn assert_locked(text: &str, json: &serde_json::Value, tables: &[&str]) {
    assert_eq!(
        locked_tables(json),
        Some(tables.iter().map(|table| (*table).to_owned()).collect()),
        "{text}"
    );
    assert!(
        text.contains(&format!("\nmetadata lock on {}:\n", tables.join(", "))),
        "{text}"
    );
    assert!(text.contains(LOCK_WARNING_STATEMENT_KINDS), "{text}");
}

/// Asserts that neither the text nor the JSON has a metadata lock warning.
fn assert_not_locked(text: &str, json: &serde_json::Value) {
    assert_eq!(locked_tables(json), None, "{text}");
    assert!(!text.contains("metadata lock on"), "{text}");
}

fn impacts_of(json: &serde_json::Value) -> Vec<&str> {
    json["sources"][0]["statements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|statement| statement["impact"].as_str().unwrap())
        .collect()
}

#[test]
fn dropping_a_table_of_the_schema_names_it_in_the_metadata_lock_warning() {
    let (text, json, code) = lock_run(&[], "drop table `orders`;");
    assert_locked(&text, &json, &["orders"]);
    assert_eq!(impacts_of(&json), ["not_checked"]);
    assert_eq!(code, Some(0));
    assert_eq!(json["exit_code"], 0);
}

#[test]
fn dropping_a_table_if_it_exists_names_it_in_the_metadata_lock_warning() {
    // Schema::dropIfExists('orders')
    let (text, json, code) = lock_run(&[], "drop table if exists `orders`;");
    assert_locked(&text, &json, &["orders"]);
    assert_eq!(impacts_of(&json), ["not_checked"]);
    assert_eq!(code, Some(0));
}

#[test]
fn dropping_a_table_in_capitals_without_backticks_names_it_too() {
    let (text, json, _) = lock_run(&[], "DROP TABLE IF EXISTS orders;");
    assert_locked(&text, &json, &["orders"]);
}

#[test]
fn dropping_a_table_without_a_schema_file_names_it_because_it_may_exist() {
    let output = kilit(&["check", "--format", "json"], "drop table `orders`;");
    let json = json_of(&output);
    assert_eq!(locked_tables(&json), Some(vec!["orders".to_owned()]));
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn several_tables_in_one_drop_are_each_named_in_the_order_they_appear() {
    let (text, json, code) = lock_run(&[], "drop table users, `orders`, customers;");
    assert_locked(&text, &json, &["users", "orders", "customers"]);
    assert_eq!(code, Some(0));
}

#[test]
fn a_table_named_twice_in_different_letter_case_is_named_once() {
    let (text, json, _) = lock_run(
        &[],
        "truncate table orders; drop table if exists users, ORDERS;",
    );
    assert_locked(&text, &json, &["orders", "users"]);
}

#[test]
fn renaming_a_table_names_the_old_name_and_never_the_new_one() {
    // Schema::rename('orders', 'orders_old')
    let (text, json, code) = lock_run(&[], "rename table `orders` to `orders_old`;");
    assert_locked(&text, &json, &["orders"]);
    assert!(!text.contains("orders_old:"), "{text}");
    assert_eq!(impacts_of(&json), ["unknown"]);
    assert_eq!(code, Some(0));
    assert_eq!(json["exit_code"], 0);
}

#[test]
fn renaming_several_tables_names_every_old_name() {
    let (text, json, _) = lock_run(
        &[],
        "rename table orders to orders_old, users to users_old;",
    );
    assert_locked(&text, &json, &["orders", "users"]);
}

#[test]
fn a_swap_rename_names_only_the_two_old_tables_and_not_the_temporary_name() {
    let (text, json, _) = lock_run(
        &[],
        "rename table orders to tmp_swap, users to orders, tmp_swap to users;",
    );
    assert_locked(&text, &json, &["orders", "users"]);
}

#[test]
fn truncating_a_table_with_and_without_the_word_table_names_it() {
    // DB::table('orders')->truncate()
    for sql in [
        "truncate table `orders`;",
        "truncate `orders`;",
        "TRUNCATE orders;",
    ] {
        let (text, json, code) = lock_run(&[], sql);
        assert_locked(&text, &json, &["orders"]);
        assert_eq!(impacts_of(&json), ["unknown"], "{sql}");
        assert_eq!(code, Some(0), "{sql}");
    }
}

#[test]
fn a_table_the_input_created_is_not_in_the_warning_when_it_is_renamed_truncated_or_dropped() {
    for sql in [
        "rename table notes to notes_old",
        "truncate table notes",
        "drop table notes",
        "drop table if exists notes",
    ] {
        let input = format!(
            "create table `notes` (`id` bigint unsigned not null auto_increment primary key, \
             `body` varchar(255) not null);\n{sql};"
        );
        let (text, json, code) = lock_run(&[], &input);
        assert_not_locked(&text, &json);
        assert_eq!(code, Some(0), "{sql}");
    }
}

#[test]
fn a_created_table_is_followed_through_a_rename_and_stays_out_of_the_warning() {
    // Migration 1 creates notes; migration 2 renames it, truncates the new
    // name and drops it.
    let input = "create table `notes` (`id` bigint unsigned not null auto_increment primary \
                 key, `body` varchar(255) not null);\n\
                 rename table `notes` to `notes_old`;\n\
                 truncate table `notes_old`;\n\
                 drop table `notes_old`;";
    let (text, json, code) = lock_run(&[], input);
    assert_not_locked(&text, &json);
    assert_eq!(code, Some(0));
}

#[test]
fn a_drop_naming_a_created_table_and_a_schema_table_names_only_the_schema_one() {
    let input = "create table `notes` (`id` bigint unsigned not null auto_increment primary \
                 key);\n\
                 drop table `notes`, `orders`;";
    let (text, json, _) = lock_run(&[], input);
    assert_locked(&text, &json, &["orders"]);
    assert!(!text.contains("notes:"), "{text}");
    assert!(!text.contains(", notes"), "{text}");
}

#[test]
fn a_schema_table_renamed_to_a_new_name_keeps_both_names_in_the_warning_when_it_is_used_again() {
    // orders existed before the input, so a transaction may use it under the
    // new name too; the new name was not created by the input.
    let (text, json, _) = lock_run(
        &[],
        "rename table orders to orders_old; truncate table orders_old;",
    );
    assert_locked(&text, &json, &["orders", "orders_old"]);
}

#[test]
fn lock_tables_gives_no_metadata_lock_warning() {
    // Measured: LOCK TABLES READ does not wait for a transaction that only
    // read the table, and readers do not queue behind it.
    for sql in [
        "lock tables orders read;",
        "LOCK TABLES `orders` WRITE;",
        "lock tables orders read, users write; unlock tables;",
    ] {
        let (text, json, code) = lock_run(&[], sql);
        assert_not_locked(&text, &json);
        assert_eq!(code, Some(0), "{sql}");
    }
}

#[test]
fn an_alter_table_still_names_its_table_and_the_warning_has_the_same_keys_as_before() {
    let (text, json, code) = lock_run(&[], "alter table orders add note varchar(255) null;");
    assert_locked(&text, &json, &["orders"]);
    assert_eq!(code, Some(0));
    let warning = json["warnings"][0].as_object().unwrap();
    let mut keys: Vec<&str> = warning.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["id", "message", "tables"]);
    assert_eq!(json["format_version"], "1.0");
}

#[test]
fn the_json_warning_text_names_the_measured_kinds_and_error_1205() {
    let (_, json, _) = lock_run(&[], "drop table orders;");
    let message = json["warnings"][0]["message"].as_str().unwrap();
    assert!(
        message.starts_with(LOCK_WARNING_STATEMENT_KINDS),
        "{message}"
    );
    assert!(
        message.contains("the statement then fails with error 1205"),
        "{message}"
    );
}

#[test]
fn drop_rename_and_truncate_keep_their_impacts_and_exit_statuses_with_and_without_strict() {
    // DROP TABLE is not checked, RENAME TABLE and TRUNCATE are unknown;
    // only --strict counts the unknown ones (exit status 3).
    for (sql, impact, strict_code) in [
        ("drop table orders;", "not_checked", 0),
        ("drop table if exists orders;", "not_checked", 0),
        ("rename table orders to orders_old;", "unknown", 3),
        ("truncate table orders;", "unknown", 3),
    ] {
        let (_, json, code) = lock_run(&[], sql);
        assert_eq!(impacts_of(&json), [impact], "{sql}");
        assert_eq!(code, Some(0), "{sql}");
        assert_eq!(json["format_version"], "1.0", "{sql}");
        assert_eq!(json["summary"]["fails"], 0, "{sql}");
        assert_eq!(json["summary"]["writes_wait"], 0, "{sql}");

        let (_, json, code) = lock_run(&["--strict"], sql);
        assert_eq!(code, Some(strict_code), "{sql}");
        assert_eq!(json["exit_code"], strict_code, "{sql}");
        assert!(locked_tables(&json).is_some(), "{sql}");
    }
}

#[test]
fn a_drop_after_a_failing_alter_does_not_change_the_failure_or_the_exit_status() {
    // The warning is added, the ALTER's answer and exit status stay.
    let (text, json, code) = lock_run(
        &[],
        "alter table orders add unique index uq (user_id, amount, nosuch);\n\
         alter table orders modify amount smallint not null;\n\
         drop table customers;",
    );
    assert_eq!(locked_tables(&json).unwrap().last().unwrap(), "customers");
    assert_eq!(code, json["exit_code"].as_i64().map(|c| c as i32));
    assert!(
        text.contains("metadata lock on orders, customers:"),
        "{text}"
    );
}

// Laravel's --pretend output of Schema::drop, Schema::dropIfExists,
// Schema::rename and DB::table('orders')->truncate(), as Laravel 12 prints it
// and as Laravel 13 prints it in its short form.

fn laravel_forms(migrations: &[(&str, &[&str])]) -> [(&'static str, String); 2] {
    [
        ("long", pretend_long(migrations)),
        ("short", pretend_short(migrations)),
    ]
}

#[test]
fn laravel_schema_drop_names_the_dropped_table_in_both_forms() {
    // Schema::drop('orders')
    let migrations: [(&str, &[&str]); 1] =
        [("2026_05_01_000001_drop_orders", &["drop table `orders`"])];
    for (form, input) in laravel_forms(&migrations) {
        let (text, json, code) = lock_run(&[], &input);
        assert_locked(&text, &json, &["orders"]);
        assert_eq!(code, Some(0), "{form}");
        assert_eq!(json["sources"].as_array().unwrap().len(), 1, "{form}");
        assert_eq!(impacts_of(&json), ["not_checked"], "{form}");
    }
}

#[test]
fn laravel_schema_drop_if_exists_names_the_dropped_table_in_both_forms() {
    // Schema::dropIfExists('orders')
    let migrations: [(&str, &[&str]); 1] = [(
        "2026_05_01_000001_drop_orders",
        &["drop table if exists `orders`"],
    )];
    for (form, input) in laravel_forms(&migrations) {
        let (text, json, code) = lock_run(&[], &input);
        assert_locked(&text, &json, &["orders"]);
        assert_eq!(code, Some(0), "{form}");
        assert_eq!(impacts_of(&json), ["not_checked"], "{form}");
    }
}

#[test]
fn two_laravel_drop_if_exists_calls_name_both_tables_in_both_forms() {
    // Schema::dropIfExists('orders'); Schema::dropIfExists('customers')
    let migrations: [(&str, &[&str]); 1] = [(
        "2026_05_01_000001_drop_two",
        &[
            "drop table if exists `orders`",
            "drop table if exists `customers`",
        ],
    )];
    for (form, input) in laravel_forms(&migrations) {
        let (text, json, code) = lock_run(&[], &input);
        assert_locked(&text, &json, &["orders", "customers"]);
        assert_eq!(code, Some(0), "{form}");
        assert_eq!(impacts_of(&json), ["not_checked", "not_checked"], "{form}");
    }
}

#[test]
fn laravel_schema_rename_names_the_old_table_only_in_both_forms() {
    // Schema::rename('orders', 'orders_old')
    let migrations: [(&str, &[&str]); 1] = [(
        "2026_05_01_000001_rename_orders",
        &["rename table `orders` to `orders_old`"],
    )];
    for (form, input) in laravel_forms(&migrations) {
        let (text, json, code) = lock_run(&[], &input);
        assert_locked(&text, &json, &["orders"]);
        assert!(!text.contains("orders_old:"), "{form}: {text}");
        assert_eq!(code, Some(0), "{form}");
        assert_eq!(impacts_of(&json), ["unknown"], "{form}");
    }
}

#[test]
fn laravel_table_truncate_names_the_table_in_both_forms() {
    // DB::table('orders')->truncate()
    let migrations: [(&str, &[&str]); 1] = [(
        "2026_05_01_000001_truncate_orders",
        &["truncate table `orders`"],
    )];
    for (form, input) in laravel_forms(&migrations) {
        let (text, json, code) = lock_run(&[], &input);
        assert_locked(&text, &json, &["orders"]);
        assert_eq!(code, Some(0), "{form}");
        assert_eq!(impacts_of(&json), ["unknown"], "{form}");
    }
}

#[test]
fn a_laravel_migration_that_creates_then_renames_truncates_and_drops_gives_no_warning() {
    // Migration 1: Schema::create('notes'). Migration 2: Schema::rename('notes',
    // 'notes_old'), DB::table('notes_old')->truncate(), Schema::drop('notes_old').
    let create = "create table `notes` (`id` bigint unsigned not null auto_increment primary \
                  key, `body` varchar(255) not null) default character set utf8mb4 collate \
                  'utf8mb4_unicode_ci'";
    let migrations: [(&str, &[&str]); 2] = [
        ("2026_05_01_000001_create_notes", &[create]),
        (
            "2026_05_01_000002_change_notes_ordered",
            &[
                "rename table `notes` to `notes_old`",
                "truncate table `notes_old`",
                "drop table `notes_old`",
            ],
        ),
    ];
    for (form, input) in laravel_forms(&migrations) {
        let (text, json, code) = lock_run(&[], &input);
        assert_not_locked(&text, &json);
        assert_eq!(code, Some(0), "{form}");
        assert_eq!(json["sources"].as_array().unwrap().len(), 2, "{form}");
        assert_eq!(
            impacts_of(&json),
            ["not_checked"],
            "{form}: the first source has the create only"
        );
        assert_eq!(
            json["sources"][1]["statements"].as_array().unwrap().len(),
            3,
            "{form}"
        );
    }
}

// Step 12b, review findings. Measured by hand on MySQL 8.0.46 and 8.4.13 with
// a transaction that had read orders open and lock_wait_timeout = 3.

#[test]
fn drop_tables_and_rename_tables_are_read_as_drop_table_and_rename_table() {
    // MySQL takes TABLES as TABLE: both waited 3.1 s, then error 1205.
    let (text, json, code) = lock_run(&[], "drop tables orders;");
    assert_locked(&text, &json, &["orders"]);
    assert_eq!(code, Some(0));

    let (text, json, code) = lock_run(&[], "rename tables orders to orders_old;");
    assert_locked(&text, &json, &["orders"]);
    assert!(!text.contains("orders_old:"), "{text}");
    assert_eq!(code, Some(0));
}

#[test]
fn index_statements_kilit_cannot_read_still_name_their_table_and_stay_unknown() {
    // Each waited, then 1205, on both servers.
    for sql in [
        "create fulltext index i on orders (status);",
        "create index i on orders (amount) invisible;",
        "drop index i on orders algorithm=inplace;",
    ] {
        let (text, json, code) = lock_run(&[], sql);
        assert_locked(&text, &json, &["orders"]);
        assert_eq!(impacts_of(&json), ["unknown"], "{sql}");
        assert_eq!(code, Some(0), "{sql}");
    }
}

#[test]
fn dropping_a_temporary_table_waits_for_nothing_and_gives_no_warning() {
    // 1051 at once, or returns at once with IF EXISTS: no wait.
    for sql in [
        "drop temporary table orders;",
        "drop temporary table if exists orders;",
    ] {
        let (text, json, code) = lock_run(&[], sql);
        assert_not_locked(&text, &json);
        assert_eq!(impacts_of(&json), ["not_checked"], "{sql}");
        assert_eq!(code, Some(0), "{sql}");
    }
}

#[test]
fn truncating_a_double_quoted_name_is_rejected_at_once_and_gives_no_warning() {
    // Both servers: error 1064 at once, so nothing waits.
    let (text, json, _) = lock_run(&[], "truncate table \"orders\";");
    assert_not_locked(&text, &json);
}
