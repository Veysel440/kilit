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
         \x20   measured as drop-column (7 lab scenarios)\n\
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
    assert_eq!(statements[0]["evidence"].as_array().unwrap().len(), 7);
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
