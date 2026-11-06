//! The SQL that Laravel 13's MySQL schema grammar generates, character for
//! character, and what kilit reads from it.
//!
//! Source: `Illuminate/Database/Schema/Grammars/MySqlGrammar.php` and
//! `Grammar.php` on the framework's 13.x branch. Each test names the
//! Blueprint call that produces the SQL.

use kilit_core::{
    Algorithm, Alter, Column, DataType, ForeignKey, ForeignKeyAction, Index, IndexKind,
    IntegerSize, Lock, Operation, Position, Rename, StatementKind, Variable,
};

/// The only statement in `sql`, which must be an ALTER TABLE on `orders`.
fn alter(sql: &str) -> Alter {
    let statements = kilit_core::parse(sql);
    assert_eq!(statements.len(), 1, "{sql}");
    match &statements[0].kind {
        StatementKind::Alter(alter) => {
            assert_eq!(alter.table, "orders", "{sql}");
            alter.clone()
        }
        other => panic!("{sql}: expected an ALTER TABLE, got {other:?}"),
    }
}

fn operations(sql: &str) -> Vec<Operation> {
    alter(sql).operations
}

fn kind(sql: &str) -> StatementKind {
    let statements = kilit_core::parse(sql);
    assert_eq!(statements.len(), 1, "{sql}");
    statements[0].kind.clone()
}

#[test]
fn string_nullable() {
    // $table->string('coupon')->nullable();
    assert_eq!(
        operations("alter table `orders` add `coupon` varchar(255) null"),
        [Operation::AddColumn {
            column: Column {
                name: "coupon".into(),
                data_type: DataType::VarChar { length: 255 },
                nullable: Some(true),
                ..Column::default()
            },
            position: None,
        }]
    );
}

#[test]
fn integer_after() {
    // $table->integer('qty')->after('amount');
    assert_eq!(
        operations("alter table `orders` add `qty` int not null after `amount`"),
        [Operation::AddColumn {
            column: Column {
                name: "qty".into(),
                data_type: DataType::Integer {
                    size: IntegerSize::Int,
                    unsigned: false,
                },
                nullable: Some(false),
                ..Column::default()
            },
            position: Some(Position::After("amount".into())),
        }]
    );
}

#[test]
fn foreign_id_with_default_and_comment() {
    // $table->foreignId('shop_id')->default(1)->comment('owner');
    let operations = operations(
        "alter table `orders` add `shop_id` bigint unsigned not null default '1' comment 'owner'",
    );
    let [Operation::AddColumn { column, .. }] = &operations[..] else {
        panic!("{operations:?}");
    };
    assert_eq!(column.data_type.to_string(), "BIGINT UNSIGNED");
    assert_eq!(column.default.as_deref(), Some("'1'"));
    assert_eq!(column.comment.as_deref(), Some("owner"));
}

#[test]
fn instant_and_lock_modifiers() {
    // $table->string('coupon')->nullable()->instant()->lock('none');
    // MySQL 8.0 and 8.4 refuse this combination with error 1221; kilit must
    // read it exactly as written to be able to say so.
    let alter =
        alter("alter table `orders` add `coupon` varchar(255) null, algorithm=instant, lock=none");
    assert_eq!(alter.algorithm, Some(Algorithm::Instant));
    assert_eq!(alter.lock, Some(Lock::None));
    assert_eq!(alter.operations.len(), 1);
}

#[test]
fn change_type() {
    // $table->bigInteger('amount')->change();
    assert_eq!(
        operations("alter table `orders` modify `amount` bigint not null"),
        [Operation::ModifyColumn {
            column: Column {
                name: "amount".into(),
                data_type: DataType::Integer {
                    size: IntegerSize::Big,
                    unsigned: false,
                },
                nullable: Some(false),
                ..Column::default()
            },
            position: None,
        }]
    );
}

#[test]
fn legacy_rename_column() {
    // $table->renameColumn('note', 'memo') on MySQL older than 8.0.3 or
    // MariaDB older than 10.5.2 (compileLegacyRenameColumn).
    let operations = operations("alter table `orders` change `note` `memo` varchar(50) null");
    assert!(matches!(
        &operations[..],
        [Operation::ChangeColumn { old_name, column, .. }]
            if old_name == "note" && column.name == "memo"
    ));
}

#[test]
fn indexes() {
    // $table->index('amount'); ->unique('email'); ->fullText('note');
    // ->spatialIndex('location'); with Laravel's generated names.
    let cases = [
        (
            "alter table `orders` add index `orders_amount_index`(`amount`)",
            "orders_amount_index",
            IndexKind::Plain,
            "amount",
        ),
        (
            "alter table `orders` add unique `orders_email_unique`(`email`)",
            "orders_email_unique",
            IndexKind::Unique,
            "email",
        ),
        (
            "alter table `orders` add fulltext `orders_note_fulltext`(`note`)",
            "orders_note_fulltext",
            IndexKind::Fulltext,
            "note",
        ),
        (
            "alter table `orders` add spatial index `orders_location_spatialindex`(`location`)",
            "orders_location_spatialindex",
            IndexKind::Spatial,
            "location",
        ),
    ];
    for (sql, name, kind, column) in cases {
        assert_eq!(
            operations(sql),
            [Operation::AddIndex(Index {
                name: Some(name.into()),
                kind,
                columns: vec![column.into()],
            })],
            "{sql}"
        );
    }
}

#[test]
fn index_with_inplace_and_lock() {
    // $table->index('amount')->inplace()->lock('none');
    let alter = alter(
        "alter table `orders` add index `orders_amount_index`(`amount`), algorithm=inplace, lock=none",
    );
    assert_eq!(alter.algorithm, Some(Algorithm::Inplace));
    assert_eq!(alter.lock, Some(Lock::None));
}

#[test]
fn primary_key() {
    // $table->primary(['id', 'user_id']);
    assert_eq!(
        operations("alter table `orders` add primary key (`id`, `user_id`)"),
        [Operation::AddPrimaryKey {
            columns: vec!["id".into(), "user_id".into()],
        }]
    );
}

#[test]
fn foreign_key_constrained() {
    // $table->foreignId('user_id')->constrained()->cascadeOnDelete();
    // (the column itself comes in a separate ADD before this one)
    assert_eq!(
        operations(
            "alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) \
             references `users` (`id`) on delete cascade"
        ),
        [Operation::AddForeignKey(ForeignKey {
            name: Some("orders_user_id_foreign".into()),
            columns: vec!["user_id".into()],
            referenced_table: "users".into(),
            referenced_columns: vec!["id".into()],
            on_delete: Some(ForeignKeyAction::Cascade),
            on_update: None,
        })]
    );
}

#[test]
fn foreign_key_inplace() {
    // $table->foreign('user_id')->references('id')->on('users')->inplace();
    let alter = alter(
        "alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) \
         references `users` (`id`), algorithm=inplace",
    );
    assert_eq!(alter.algorithm, Some(Algorithm::Inplace));
    assert!(matches!(
        alter.operations[..],
        [Operation::AddForeignKey(_)]
    ));
}

#[test]
fn drops() {
    // $table->dropColumn(['note', 'code']);
    assert_eq!(
        operations("alter table `orders` drop `note`, drop `code`"),
        [
            Operation::DropColumn {
                name: "note".into()
            },
            Operation::DropColumn {
                name: "code".into()
            },
        ]
    );
    // $table->dropIndex(...); $table->dropUnique(...) generate the same SQL.
    assert_eq!(
        operations("alter table `orders` drop index `orders_amount_index`"),
        [Operation::DropIndex {
            name: "orders_amount_index".into()
        }]
    );
    // $table->dropForeign(['user_id']);
    assert_eq!(
        operations("alter table `orders` drop foreign key `orders_user_id_foreign`"),
        [Operation::DropForeignKey {
            name: "orders_user_id_foreign".into()
        }]
    );
    // $table->dropPrimary();
    assert_eq!(
        operations("alter table `orders` drop primary key"),
        [Operation::DropPrimaryKey]
    );
}

#[test]
fn tables() {
    // Schema::rename('orders', 'purchases');
    assert_eq!(
        kind("rename table `orders` to `purchases`"),
        StatementKind::RenameTable {
            renames: vec![Rename {
                from: "orders".into(),
                to: "purchases".into(),
            }],
        }
    );
    // Schema::dropIfExists('orders');
    assert_eq!(
        kind("drop table if exists `orders`"),
        StatementKind::DropTable {
            tables: vec!["orders".into()],
        }
    );
    // Schema::create('shops', function ($table) { $table->id(); $table->string('name'); });
    let StatementKind::CreateTable {
        table,
        if_not_exists: false,
    } = kind(
        "create table `shops` (`id` bigint unsigned not null auto_increment primary key, \
         `name` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'",
    )
    else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(table.name, "shops");
    assert_eq!(table.primary_key, ["id"]);
    let id = &table.columns[0];
    assert!(id.auto_increment && id.other_options.is_empty());
    assert_eq!(
        table.columns[1].data_type,
        DataType::VarChar { length: 255 }
    );
    assert_eq!(table.charset.as_deref(), Some("utf8mb4"));
    assert_eq!(table.collation.as_deref(), Some("utf8mb4_unicode_ci"));
}

#[test]
fn foreign_key_checks() {
    // Schema::disableForeignKeyConstraints(); the grammar's string ends
    // with its own semicolon.
    let statements = kilit_core::parse("SET FOREIGN_KEY_CHECKS=0;");
    assert_eq!(statements.len(), 1);
    assert_eq!(
        statements[0].kind,
        StatementKind::Set {
            variables: vec![Variable {
                name: "foreign_key_checks".into(),
                value: "0".into(),
                global: false,
            }],
        }
    );
}

#[test]
fn rename_index_is_not_readable_yet() {
    // $table->renameIndex('a', 'b'); sqlparser 0.63 cannot read RENAME INDEX.
    assert!(matches!(
        kind("alter table `orders` rename index `a` to `b`"),
        StatementKind::Unparsed { .. }
    ));
}

#[test]
fn rename_column() {
    // $table->renameColumn('note', 'memo');
    assert_eq!(
        operations("alter table `orders` rename column `note` to `memo`"),
        [Operation::RenameColumn {
            from: "note".into(),
            to: "memo".into(),
        }]
    );
}

// `php artisan migrate --pretend` for foreign keys, byte for byte as Laravel
// 12.71.0 prints it (the same bytes in Laravel 13.40.0 in a terminal) and as
// Laravel 13.40.0 prints it where it detects an automated environment, a
// shorter form. The schema calls are in the comments.

use kilit_core::{Knowledge, Prediction, Schema, Verdict, check, read_pretend};

const USERS: &str = "create table `users` (`id` bigint unsigned not null auto_increment, \
    `name` varchar(255) collate utf8mb4_unicode_ci not null, primary key (`id`)) \
    engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n";
const ORDERS: &str = "create table `orders` (`id` bigint unsigned not null auto_increment, \
    primary key (`id`)) engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n";
const CHILDREN: &str = "create table `children` (`id` bigint unsigned not null auto_increment, \
    primary key (`id`)) engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n";

const COPY: &str = "COPY, copies every row, writes wait";
const INSTANT: &str = "INSTANT, no rebuild, writes continue";
const INPLACE: &str = "INPLACE, no rebuild, writes continue";
const NOT_CHECKED: &str = "not checked";

/// What kilit says about every statement of `output` on `server`, with the
/// tables of `schema`.
fn answers(server: &str, schema: &str, output: &str) -> Vec<String> {
    let pretend = read_pretend(output).expect("the output of migrate --pretend");
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    let statements: Vec<_> = pretend
        .migrations
        .into_iter()
        .flat_map(|migration| migration.statements)
        .collect();
    let results = Knowledge::built_in().server(server).unwrap();
    check(&statements, &Schema::from_sql(schema), results)
        .into_iter()
        .map(|finding| match finding.prediction {
            Prediction::Measured {
                verdict: Verdict::Fails { code, .. },
                ..
            } => format!("fails with error {code}"),
            Prediction::Measured { verdict, .. } => verdict.to_string(),
            Prediction::Unknown { .. } => "unknown".to_owned(),
            Prediction::NotChecked { .. } => NOT_CHECKED.to_owned(),
        })
        .collect()
}

/// The long and the short form of one migration's output, and the answers
/// on MySQL 8.0.46 and 8.4.13.
struct Run {
    name: &'static str,
    long: &'static str,
    short: &'static str,
    schema: String,
    on_80: Vec<&'static str>,
    on_84: Vec<&'static str>,
}

fn assert_both_forms(run: Run) {
    for (form, output) in [("long", run.long), ("short", run.short)] {
        assert_eq!(
            answers("mysql-8.0.46", &run.schema, output),
            run.on_80,
            "{} on 8.0.46, {form} form",
            run.name
        );
        assert_eq!(
            answers("mysql-8.4.13", &run.schema, output),
            run.on_84,
            "{} on 8.4.13, {form} form",
            run.name
        );
    }
}

const CASE1A_CREATE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_orders_table ......................................  \n",
    "  \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` int not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`)  \n",
    "\n",
);

const CASE1A_CREATE_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_create_orders_table .. \n",
    " \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` int not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`) \n",
    "\n",
);

const CASE1B_TABLE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_add_user_id_to_orders_table ..............................  \n",
    "  \u{21c2} alter table `orders` add `user_id` int not null  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`)  \n",
    "\n",
);

const CASE1B_TABLE_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_add_user_id_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `user_id` int not null \n",
    " \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`) \n",
    "\n",
);

const CASE2_CREATE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_parents_table .....................................  \n",
    "  \u{21c2} create table `parents` (`id` int unsigned not null auto_increment primary key) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  2026_01_01_000002_create_children_table ....................................  \n",
    "  \u{21c2} create table `children` (`id` bigint unsigned not null auto_increment primary key, `parent_id` bigint unsigned not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  \u{21c2} alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`)  \n",
    "\n",
);

const CASE2_CREATE_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_create_parents_table .. \n",
    " \u{21c2} create table `parents` (`id` int unsigned not null auto_increment primary key) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " 2026_01_01_000002_create_children_table .. \n",
    " \u{21c2} create table `children` (`id` bigint unsigned not null auto_increment primary key, `parent_id` bigint unsigned not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " \u{21c2} alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`) \n",
    "\n",
);

const CASE2_TABLE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_parents_table .....................................  \n",
    "  \u{21c2} create table `parents` (`id` int unsigned not null auto_increment primary key) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  2026_01_01_000002_add_parent_id_to_children_table ..........................  \n",
    "  \u{21c2} alter table `children` add `parent_id` bigint unsigned not null  \n",
    "  \u{21c2} alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`)  \n",
    "\n",
);

const CASE2_TABLE_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_create_parents_table .. \n",
    " \u{21c2} create table `parents` (`id` int unsigned not null auto_increment primary key) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " 2026_01_01_000002_add_parent_id_to_children_table .. \n",
    " \u{21c2} alter table `children` add `parent_id` bigint unsigned not null \n",
    " \u{21c2} alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`) \n",
    "\n",
);

const CASE3A_NO_INDEX_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_add_user_name_to_orders_table ............................  \n",
    "  \u{21c2} alter table `orders` add `user_name` varchar(255) null  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_user_name_foreign` foreign key (`user_name`) references `users` (`name`)  \n",
    "\n",
);

const CASE3A_NO_INDEX_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_add_user_name_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `user_name` varchar(255) null \n",
    " \u{21c2} alter table `orders` add constraint `orders_user_name_foreign` foreign key (`user_name`) references `users` (`name`) \n",
    "\n",
);

const CASE3B_INDEX_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_add_index_to_users_name ..................................  \n",
    "  \u{21c2} alter table `users` add index `users_name_index`(`name`)  \n",
    "  2026_01_01_000002_add_user_name_to_orders_table ............................  \n",
    "  \u{21c2} alter table `orders` add `user_name` varchar(255) null  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_user_name_foreign` foreign key (`user_name`) references `users` (`name`)  \n",
    "\n",
);

const CASE3B_INDEX_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_add_index_to_users_name .. \n",
    " \u{21c2} alter table `users` add index `users_name_index`(`name`) \n",
    " 2026_01_01_000002_add_user_name_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `user_name` varchar(255) null \n",
    " \u{21c2} alter table `orders` add constraint `orders_user_name_foreign` foreign key (`user_name`) references `users` (`name`) \n",
    "\n",
);

const CASE3C_UNIQUE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_add_unique_to_users_name .................................  \n",
    "  \u{21c2} alter table `users` add unique `users_name_unique`(`name`)  \n",
    "  2026_01_01_000002_add_user_name_to_orders_table ............................  \n",
    "  \u{21c2} alter table `orders` add `user_name` varchar(255) null  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_user_name_foreign` foreign key (`user_name`) references `users` (`name`)  \n",
    "\n",
);

const CASE3C_UNIQUE_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_add_unique_to_users_name .. \n",
    " \u{21c2} alter table `users` add unique `users_name_unique`(`name`) \n",
    " 2026_01_01_000002_add_user_name_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `user_name` varchar(255) null \n",
    " \u{21c2} alter table `orders` add constraint `orders_user_name_foreign` foreign key (`user_name`) references `users` (`name`) \n",
    "\n",
);

const CASE4A_CREATE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_orders_table ......................................  \n",
    "  \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`)  \n",
    "\n",
);

const CASE4A_CREATE_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_create_orders_table .. \n",
    " \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`) \n",
    "\n",
);

const CASE4B_TABLE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_add_user_id_to_orders_table ..............................  \n",
    "  \u{21c2} alter table `orders` add `user_id` bigint unsigned not null  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`)  \n",
    "\n",
);

const CASE4B_TABLE_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_add_user_id_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `user_id` bigint unsigned not null \n",
    " \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`) \n",
    "\n",
);

#[test]
fn an_integer_column_that_references_users_id_fails_with_3780_in_create_and_in_table() {
    // Schema::create('orders', ...): $table->id(); $table->integer('user_id');
    //     $table->foreign('user_id')->references('id')->on('users');
    assert_both_forms(Run {
        name: "create",
        long: CASE1A_CREATE_LONG,
        short: CASE1A_CREATE_SHORT,
        schema: USERS.to_owned(),
        on_80: vec![NOT_CHECKED, "fails with error 3780"],
        on_84: vec![NOT_CHECKED, "fails with error 3780"],
    });
    // Schema::table('orders', ...): the same two calls on an existing table.
    assert_both_forms(Run {
        name: "table",
        long: CASE1B_TABLE_LONG,
        short: CASE1B_TABLE_SHORT,
        schema: format!("{USERS}{ORDERS}"),
        on_80: vec![INSTANT, "fails with error 3780"],
        on_84: vec![INSTANT, "fails with error 3780"],
    });
}

#[test]
fn a_foreign_id_to_an_increments_parent_fails_with_3780() {
    // Schema::create('parents', ...): $table->increments('id');
    // Schema::create('children', ...): $table->id(); $table->foreignId('parent_id')->constrained();
    assert_both_forms(Run {
        name: "create parents and children",
        long: CASE2_CREATE_LONG,
        short: CASE2_CREATE_SHORT,
        schema: String::new(),
        on_80: vec![NOT_CHECKED, NOT_CHECKED, "fails with error 3780"],
        on_84: vec![NOT_CHECKED, NOT_CHECKED, "fails with error 3780"],
    });
    // The same foreignId added to a children table that exists.
    assert_both_forms(Run {
        name: "parents, then a column on children",
        long: CASE2_TABLE_LONG,
        short: CASE2_TABLE_SHORT,
        schema: CHILDREN.to_owned(),
        on_80: vec![NOT_CHECKED, INSTANT, "fails with error 3780"],
        on_84: vec![NOT_CHECKED, INSTANT, "fails with error 3780"],
    });
}

#[test]
fn a_foreign_key_to_a_parent_column_without_an_index_fails_with_1822_on_mysql_80_and_6125_on_mysql_84()
 {
    // $table->string('user_name')->nullable();
    // $table->foreign('user_name')->references('name')->on('users');
    assert_both_forms(Run {
        name: "no index on users.name",
        long: CASE3A_NO_INDEX_LONG,
        short: CASE3A_NO_INDEX_SHORT,
        schema: format!("{USERS}{ORDERS}"),
        on_80: vec![INSTANT, "fails with error 1822"],
        on_84: vec![INSTANT, "fails with error 6125"],
    });
}

#[test]
fn a_foreign_key_to_a_column_with_a_plain_index_is_accepted_by_mysql_80_and_fails_with_6125_on_mysql_84()
 {
    // Schema::table('users', ...): $table->index('name');
    // then the string column and its foreign key on orders, in a second migration.
    assert_both_forms(Run {
        name: "plain index on users.name",
        long: CASE3B_INDEX_LONG,
        short: CASE3B_INDEX_SHORT,
        schema: format!("{USERS}{ORDERS}"),
        on_80: vec![INPLACE, INSTANT, COPY],
        on_84: vec![INPLACE, INSTANT, "fails with error 6125"],
    });
}

#[test]
fn a_foreign_key_to_a_column_with_a_unique_index_copies_the_table_on_both_servers() {
    // Schema::table('users', ...): $table->unique('name'); then the same as above.
    assert_both_forms(Run {
        name: "unique index on users.name",
        long: CASE3C_UNIQUE_LONG,
        short: CASE3C_UNIQUE_SHORT,
        schema: format!("{USERS}{ORDERS}"),
        on_80: vec![INPLACE, INSTANT, COPY],
        on_84: vec![INPLACE, INSTANT, COPY],
    });
}

#[test]
fn a_foreign_id_constrained_to_users_keeps_its_answer_on_both_servers() {
    // $table->foreignId('user_id')->constrained(), in Schema::create and in Schema::table.
    assert_both_forms(Run {
        name: "foreignId in create",
        long: CASE4A_CREATE_LONG,
        short: CASE4A_CREATE_SHORT,
        schema: USERS.to_owned(),
        on_80: vec![NOT_CHECKED, COPY],
        on_84: vec![NOT_CHECKED, COPY],
    });
    assert_both_forms(Run {
        name: "foreignId in table",
        long: CASE4B_TABLE_LONG,
        short: CASE4B_TABLE_SHORT,
        schema: format!("{USERS}{ORDERS}"),
        on_80: vec![INSTANT, COPY],
        on_84: vec![INSTANT, COPY],
    });
}

#[test]
fn the_short_form_and_the_long_form_give_kilit_the_same_statements() {
    for (long, short) in [
        (CASE1A_CREATE_LONG, CASE1A_CREATE_SHORT),
        (CASE2_CREATE_LONG, CASE2_CREATE_SHORT),
        (CASE3B_INDEX_LONG, CASE3B_INDEX_SHORT),
    ] {
        let sql = |output: &str| -> Vec<String> {
            read_pretend(output)
                .unwrap()
                .migrations
                .into_iter()
                .flat_map(|migration| migration.statements)
                .map(|statement| statement.sql)
                .collect()
        };
        assert_eq!(sql(long), sql(short));
    }
}

#[test]
fn a_foreign_id_that_is_not_nullable_with_null_on_delete_is_unknown_on_both_servers() {
    // $table->foreignId('user_id')->constrained()->nullOnDelete(); the column is
    // NOT NULL, so MySQL refuses the key before it looks at anything else.
    // Laravel adds ` on delete set null` to the foreign key's statement.
    let output = concat!(
        "\n",
        "   INFO  Running migrations.  \n",
        "\n",
        "  2026_01_01_000001_add_user_id_to_orders_table ..............................  \n",
        "  \u{21c2} alter table `orders` add `user_id` bigint unsigned not null  \n",
        "  \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`) on delete set null  \n",
        "\n",
    );
    let schema = format!("{USERS}{ORDERS}");
    for server in ["mysql-8.0.46", "mysql-8.4.13"] {
        assert_eq!(answers(server, &schema, output), [INSTANT, "unknown"]);
    }
}

// Step 9a: Laravel and the storage engine. `Schema::create` writes the
// engine last, as ` engine = MyISAM`, with spaces around the equals sign and
// the name as the project wrote it. `Schema::table` never writes an engine,
// so an ALTER from Laravel says nothing about the engine of its table. The
// outputs below are what Laravel 12.71.0 prints (the same bytes in Laravel
// 13.40.0 in a terminal) and what Laravel 13.40.0 prints where it detects an
// automated environment. Laravel 10 and 11 run the same code for these
// statements. The migrations that put several steps in one output are the
// real outputs of the steps, joined as one `migrate --pretend` run prints
// them.

const MYISAM_CREATE_FK_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_orders_table ......................................  \n",
    "  \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = MyISAM  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`)  \n",
    "\n",
);

const MYISAM_CREATE_FK_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_create_orders_table .. \n",
    " \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = MyISAM \n",
    " \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`) \n",
    "\n",
);

const MYISAM_ADD_QTY_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_add_qty_to_orders_table ..................................  \n",
    "  \u{21c2} alter table `orders` add `qty` int null  \n",
    "\n",
);

const MYISAM_ADD_QTY_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_add_qty_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `qty` int null \n",
    "\n",
);

const MYISAM_ADD_INDEX_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_add_index_to_orders_table ................................  \n",
    "  \u{21c2} alter table `orders` add index `orders_amount_index`(`amount`)  \n",
    "\n",
);

const MYISAM_ADD_INDEX_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_add_index_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add index `orders_amount_index`(`amount`) \n",
    "\n",
);

const MYISAM_FOREIGN_ID_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_add_user_id_to_orders_table ..............................  \n",
    "  \u{21c2} alter table `orders` add `user_id` bigint unsigned not null  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`)  \n",
    "\n",
);

const MYISAM_FOREIGN_ID_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_add_user_id_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `user_id` bigint unsigned not null \n",
    " \u{21c2} alter table `orders` add constraint `orders_user_id_foreign` foreign key (`user_id`) references `users` (`id`) \n",
    "\n",
);

const MYISAM_INTEGER_FOREIGN_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_add_ref_to_orders_table ..................................  \n",
    "  \u{21c2} alter table `orders` add `ref` int not null  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_ref_foreign` foreign key (`ref`) references `users` (`id`)  \n",
    "\n",
);

const MYISAM_INTEGER_FOREIGN_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_add_ref_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `ref` int not null \n",
    " \u{21c2} alter table `orders` add constraint `orders_ref_foreign` foreign key (`ref`) references `users` (`id`) \n",
    "\n",
);

const MYISAM_NULLABLE_FOREIGN_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_add_ref_to_orders_table ..................................  \n",
    "  \u{21c2} alter table `orders` add `ref` int null  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_ref_foreign` foreign key (`ref`) references `users` (`id`)  \n",
    "\n",
);

const MYISAM_NULLABLE_FOREIGN_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_add_ref_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `ref` int null \n",
    " \u{21c2} alter table `orders` add constraint `orders_ref_foreign` foreign key (`ref`) references `users` (`id`) \n",
    "\n",
);

const MYISAM_LOWERCASE_CREATE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_orders_table ......................................  \n",
    "  \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = myisam  \n",
    "\n",
);

const MYISAM_LOWERCASE_CREATE_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_create_orders_table .. \n",
    " \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = myisam \n",
    "\n",
);

const INNODB_CREATE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_orders_table ......................................  \n",
    "  \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = InnoDB  \n",
    "\n",
);

const INNODB_CREATE_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_create_orders_table .. \n",
    " \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = InnoDB \n",
    "\n",
);

const ENGINE_IN_SCHEMA_TABLE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_add_qty_to_orders_table ..................................  \n",
    "  \u{21c2} alter table `orders` add `qty` int null  \n",
    "\n",
);

const ENGINE_IN_SCHEMA_TABLE_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_add_qty_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `qty` int null \n",
    "\n",
);

const MYISAM_CREATE_THEN_INTEGER_FOREIGN_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_orders_table ......................................  \n",
    "  \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = MyISAM  \n",
    "  2026_01_01_000002_add_ref_to_orders_table ..................................  \n",
    "  \u{21c2} alter table `orders` add `ref` int not null  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_ref_foreign` foreign key (`ref`) references `users` (`id`)  \n",
    "\n",
);

const MYISAM_CREATE_THEN_INTEGER_FOREIGN_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_create_orders_table .. \n",
    " \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = MyISAM \n",
    " 2026_01_01_000002_add_ref_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `ref` int not null \n",
    " \u{21c2} alter table `orders` add constraint `orders_ref_foreign` foreign key (`ref`) references `users` (`id`) \n",
    "\n",
);

const MYISAM_CREATE_THEN_NULLABLE_FOREIGN_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_orders_table ......................................  \n",
    "  \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = MyISAM  \n",
    "  2026_01_01_000002_add_ref_to_orders_table ..................................  \n",
    "  \u{21c2} alter table `orders` add `ref` int null  \n",
    "  \u{21c2} alter table `orders` add constraint `orders_ref_foreign` foreign key (`ref`) references `users` (`id`)  \n",
    "\n",
);

const MYISAM_CREATE_THEN_NULLABLE_FOREIGN_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_create_orders_table .. \n",
    " \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = MyISAM \n",
    " 2026_01_01_000002_add_ref_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `ref` int null \n",
    " \u{21c2} alter table `orders` add constraint `orders_ref_foreign` foreign key (`ref`) references `users` (`id`) \n",
    "\n",
);

const MYISAM_CREATE_THEN_ADD_QTY_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_orders_table ......................................  \n",
    "  \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = MyISAM  \n",
    "  2026_01_01_000002_add_qty_to_orders_table ..................................  \n",
    "  \u{21c2} alter table `orders` add `qty` int null  \n",
    "\n",
);

const MYISAM_CREATE_THEN_ADD_QTY_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_create_orders_table .. \n",
    " \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = MyISAM \n",
    " 2026_01_01_000002_add_qty_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `qty` int null \n",
    "\n",
);

const MYISAM_LOWERCASE_CREATE_THEN_ADD_QTY_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_orders_table ......................................  \n",
    "  \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = myisam  \n",
    "  2026_01_01_000002_add_qty_to_orders_table ..................................  \n",
    "  \u{21c2} alter table `orders` add `qty` int null  \n",
    "\n",
);

const INNODB_CREATE_THEN_ADD_QTY_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_01_01_000001_create_orders_table ......................................  \n",
    "  \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = InnoDB  \n",
    "  2026_01_01_000002_add_qty_to_orders_table ..................................  \n",
    "  \u{21c2} alter table `orders` add `qty` int null  \n",
    "\n",
);

const INNODB_CREATE_THEN_ADD_QTY_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_01_01_000001_create_orders_table .. \n",
    " \u{21c2} create table `orders` (`id` bigint unsigned not null auto_increment primary key, `user_id` bigint unsigned not null, `amount` int not null, `email` varchar(255) null, `note` varchar(50) null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' engine = InnoDB \n",
    " 2026_01_01_000002_add_qty_to_orders_table .. \n",
    " \u{21c2} alter table `orders` add `qty` int null \n",
    "\n",
);

use kilit_core::{Engine, Statement};

/// `orders` as a mysqldump schema writes a MyISAM table.
const MYISAM_ORDERS: &str = "create table `orders` (`id` bigint unsigned not null auto_increment, \
    `amount` int not null, primary key (`id`)) \
    engine=MyISAM default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n";

/// The statements of `output`, in order, whichever form it is in.
fn statements_of(output: &str) -> Vec<Statement> {
    let pretend = read_pretend(output).expect("the output of migrate --pretend");
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    pretend
        .migrations
        .into_iter()
        .flat_map(|migration| migration.statements)
        .collect()
}

/// The engine the CREATE TABLE that comes first in `output` writes.
fn created_engine(output: &str) -> Option<Engine> {
    match &statements_of(output)[0].kind {
        StatementKind::CreateTable { table, .. } => table.engine.clone(),
        other => panic!("expected a CREATE TABLE, got {other:?}"),
    }
}

#[test]
fn laravel_s_engine_clause_is_read_in_every_form_it_prints() {
    // $table->engine('MyISAM'), $table->engine = 'MyISAM', and the
    // connection's 'engine' => 'MyISAM': the same text.
    for output in [MYISAM_CREATE_FK_LONG, MYISAM_CREATE_FK_SHORT] {
        assert_eq!(created_engine(output), Some(Engine::MyIsam));
    }
    // $table->engine('myisam');
    for output in [MYISAM_LOWERCASE_CREATE_LONG, MYISAM_LOWERCASE_CREATE_SHORT] {
        assert_eq!(created_engine(output), Some(Engine::MyIsam));
    }
    // $table->innoDb();
    for output in [INNODB_CREATE_LONG, INNODB_CREATE_SHORT] {
        assert_eq!(created_engine(output), Some(Engine::InnoDb));
    }
    // A connection without charset and collation: the engine follows the
    // closing parenthesis. And no engine anywhere.
    let StatementKind::CreateTable { table, .. } = kind(
        "create table `orders` (`id` bigint unsigned not null auto_increment primary key, \
         `amount` int not null) engine = MyISAM",
    ) else {
        panic!("expected a CREATE TABLE");
    };
    assert_eq!(table.engine, Some(Engine::MyIsam));
    let StatementKind::CreateTable { table, .. } = kind(
        "create table `orders` (`id` bigint unsigned not null auto_increment primary key, \
         `amount` int not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'",
    ) else {
        panic!("expected a CREATE TABLE");
    };
    assert_eq!(table.engine, None);
}

#[test]
fn a_myisam_create_followed_by_its_foreign_key_does_not_fail_in_laravel_12_and_13() {
    // Schema::create('orders', ...): $table->engine('MyISAM'); $table->id();
    //     $table->foreignId('user_id')->constrained();
    // The same two statements come out with 'engine' => 'MyISAM' in the
    // connection's config. Laravel writes no foreign key in the CREATE.
    assert_both_forms(Run {
        name: "MyISAM create with foreignId",
        long: MYISAM_CREATE_FK_LONG,
        short: MYISAM_CREATE_FK_SHORT,
        schema: USERS.to_owned(),
        on_80: vec![NOT_CHECKED, COPY],
        on_84: vec![NOT_CHECKED, COPY],
    });
}

#[test]
fn an_integer_foreign_key_on_a_myisam_table_created_earlier_copies_instead_of_failing_with_3780() {
    // Schema::create('orders', ...): $table->engine('MyISAM');
    // then, in another migration, Schema::table('orders', ...):
    //     $table->integer('ref'); $table->foreign('ref')->references('id')->on('users');
    // On InnoDB the key fails with 3780: INT against BIGINT UNSIGNED.
    assert_both_forms(Run {
        name: "NOT NULL integer",
        long: MYISAM_CREATE_THEN_INTEGER_FOREIGN_LONG,
        short: MYISAM_CREATE_THEN_INTEGER_FOREIGN_SHORT,
        schema: USERS.to_owned(),
        on_80: vec![NOT_CHECKED, COPY, COPY],
        on_84: vec![NOT_CHECKED, COPY, COPY],
    });
    // $table->integer('ref')->nullable();
    assert_both_forms(Run {
        name: "nullable integer",
        long: MYISAM_CREATE_THEN_NULLABLE_FOREIGN_LONG,
        short: MYISAM_CREATE_THEN_NULLABLE_FOREIGN_SHORT,
        schema: USERS.to_owned(),
        on_80: vec![NOT_CHECKED, COPY, COPY],
        on_84: vec![NOT_CHECKED, COPY, COPY],
    });
}

#[test]
fn a_column_added_to_a_myisam_table_created_earlier_copies_the_table() {
    // $table->integer('qty')->nullable(); after Schema::create with
    // $table->engine('MyISAM') (and with engine('myisam')).
    assert_both_forms(Run {
        name: "engine('MyISAM')",
        long: MYISAM_CREATE_THEN_ADD_QTY_LONG,
        short: MYISAM_CREATE_THEN_ADD_QTY_SHORT,
        schema: String::new(),
        on_80: vec![NOT_CHECKED, COPY],
        on_84: vec![NOT_CHECKED, COPY],
    });
    for server in ["mysql-8.0.46", "mysql-8.4.13"] {
        assert_eq!(
            answers(server, "", MYISAM_LOWERCASE_CREATE_THEN_ADD_QTY_LONG),
            [NOT_CHECKED, COPY],
            "engine('myisam') on {server}"
        );
    }
}

#[test]
fn laravel_statements_on_a_myisam_table_from_the_schema_file_get_the_myisam_answers() {
    // Schema::table never writes the engine: it comes from the schema file.
    let schema = format!("{USERS}{MYISAM_ORDERS}");
    // $table->integer('qty')->nullable();
    assert_both_forms(Run {
        name: "add a nullable column",
        long: MYISAM_ADD_QTY_LONG,
        short: MYISAM_ADD_QTY_SHORT,
        schema: schema.clone(),
        on_80: vec![COPY],
        on_84: vec![COPY],
    });
    // $table->index('amount');
    assert_both_forms(Run {
        name: "add an index",
        long: MYISAM_ADD_INDEX_LONG,
        short: MYISAM_ADD_INDEX_SHORT,
        schema: schema.clone(),
        on_80: vec![COPY],
        on_84: vec![COPY],
    });
    // $table->foreignId('user_id')->constrained();
    assert_both_forms(Run {
        name: "foreignId",
        long: MYISAM_FOREIGN_ID_LONG,
        short: MYISAM_FOREIGN_ID_SHORT,
        schema: schema.clone(),
        on_80: vec![COPY, COPY],
        on_84: vec![COPY, COPY],
    });
    // $table->integer('ref'); $table->foreign('ref')->references('id')->on('users');
    assert_both_forms(Run {
        name: "NOT NULL integer foreign key",
        long: MYISAM_INTEGER_FOREIGN_LONG,
        short: MYISAM_INTEGER_FOREIGN_SHORT,
        schema: schema.clone(),
        on_80: vec![COPY, COPY],
        on_84: vec![COPY, COPY],
    });
    // The same with ->nullable().
    assert_both_forms(Run {
        name: "nullable integer foreign key",
        long: MYISAM_NULLABLE_FOREIGN_LONG,
        short: MYISAM_NULLABLE_FOREIGN_SHORT,
        schema: schema.clone(),
        on_80: vec![COPY, COPY],
        on_84: vec![COPY, COPY],
    });
    // $table->engine('MyISAM') inside Schema::table is dropped by Laravel: the
    // statement is the plain ADD COLUMN, on a MyISAM table here.
    assert_both_forms(Run {
        name: "engine() in Schema::table",
        long: ENGINE_IN_SCHEMA_TABLE_LONG,
        short: ENGINE_IN_SCHEMA_TABLE_SHORT,
        schema,
        on_80: vec![COPY],
        on_84: vec![COPY],
    });
}

#[test]
fn the_same_laravel_statements_on_an_innodb_table_keep_their_answers() {
    let schema = format!("{USERS}{ORDERS}");
    // engine() in Schema::table, and no engine at all: an InnoDB table.
    assert_both_forms(Run {
        name: "engine() in Schema::table",
        long: ENGINE_IN_SCHEMA_TABLE_LONG,
        short: ENGINE_IN_SCHEMA_TABLE_SHORT,
        schema: schema.clone(),
        on_80: vec![INSTANT],
        on_84: vec![INSTANT],
    });
    assert_both_forms(Run {
        name: "add a nullable column",
        long: MYISAM_ADD_QTY_LONG,
        short: MYISAM_ADD_QTY_SHORT,
        schema,
        on_80: vec![INSTANT],
        on_84: vec![INSTANT],
    });
    // $table->innoDb() on a new table.
    assert_both_forms(Run {
        name: "innoDb() create",
        long: INNODB_CREATE_THEN_ADD_QTY_LONG,
        short: INNODB_CREATE_THEN_ADD_QTY_SHORT,
        schema: String::new(),
        on_80: vec![NOT_CHECKED, INSTANT],
        on_84: vec![NOT_CHECKED, INSTANT],
    });
    // The integer foreign key still fails when the table is InnoDB.
    assert_both_forms(Run {
        name: "integer foreign key",
        long: MYISAM_INTEGER_FOREIGN_LONG,
        short: MYISAM_INTEGER_FOREIGN_SHORT,
        schema: format!("{USERS}{ORDERS}"),
        on_80: vec![INSTANT, "fails with error 3780"],
        on_84: vec![INSTANT, "fails with error 3780"],
    });
}

#[test]
fn the_short_and_the_long_form_of_the_engine_outputs_give_the_same_statements() {
    for (long, short) in [
        (MYISAM_CREATE_FK_LONG, MYISAM_CREATE_FK_SHORT),
        (MYISAM_ADD_QTY_LONG, MYISAM_ADD_QTY_SHORT),
        (MYISAM_ADD_INDEX_LONG, MYISAM_ADD_INDEX_SHORT),
        (MYISAM_FOREIGN_ID_LONG, MYISAM_FOREIGN_ID_SHORT),
        (MYISAM_INTEGER_FOREIGN_LONG, MYISAM_INTEGER_FOREIGN_SHORT),
        (MYISAM_NULLABLE_FOREIGN_LONG, MYISAM_NULLABLE_FOREIGN_SHORT),
        (
            MYISAM_CREATE_THEN_INTEGER_FOREIGN_LONG,
            MYISAM_CREATE_THEN_INTEGER_FOREIGN_SHORT,
        ),
        (
            MYISAM_CREATE_THEN_NULLABLE_FOREIGN_LONG,
            MYISAM_CREATE_THEN_NULLABLE_FOREIGN_SHORT,
        ),
        (
            MYISAM_CREATE_THEN_ADD_QTY_LONG,
            MYISAM_CREATE_THEN_ADD_QTY_SHORT,
        ),
        (INNODB_CREATE_LONG, INNODB_CREATE_SHORT),
        (ENGINE_IN_SCHEMA_TABLE_LONG, ENGINE_IN_SCHEMA_TABLE_SHORT),
    ] {
        let sql = |output: &str| -> Vec<String> {
            statements_of(output)
                .into_iter()
                .map(|statement| statement.sql)
                .collect()
        };
        assert_eq!(sql(long), sql(short));
    }
}

// ---------------------------------------------------------------------------
// Laravel's error screen and migrations that read the database.
//
// The inputs are `php artisan migrate --pretend` runs of hand-written
// migrations: Laravel 12.71.0 and 13.40.0 (the long form, and the short form
// 13.40.0 prints where it detects an automated environment), with Collision
// (the default in a new application) and without it (Symfony's error screen),
// piped, with `--ansi`, and in a terminal (CRLF line ends and colors, `_PTY`).
// `COLS40`, `COLS48` and `COLS50` ran with COLUMNS set, so Laravel fits each
// name line to the terminal. `DERIVED_L10_*` and `DERIVED_L11_*` were not run:
// Laravel 10 and 11 cannot be installed here, so they are the Laravel 12
// output with the lines whose framework source (v10.50.4 and v11.57.1)
// differs replaced. Some tests edit a real output (cut a name line, add a
// statement); each says so.
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

const C1_THROW_L12_ANSI: &str = concat!(
    "\n",
    "  \u{1b}[37;44m INFO \u{1b}[39;49m Running migrations.  \n",
    "\n",
    "  2026_02_01_000001_throw_after_alter_orders_table \u{1b}[90m...........................\u{1b}[39m  \n",
    "\n",
    "  \u{1b}[41;1m RuntimeException \u{1b}[49;22m\n",
    "\n",
    "\u{1b}[39;1m  boom from a migration\u{1b}[39;22m\n",
    "\n",
    "  at \u{1b}[32mdatabase/migrations/step10/c1_throw/2026_02_01_000001_throw_after_alter_orders_table.php\u{1b}[39m:\u{1b}[32m15\u{1b}[39m\n",
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
    "  \u{1b}[33m28  \u{1b}[39m\u{1b}[39;1martisan\u{1b}[39;22m:\u{1b}[39;1m16\u{1b}[39;22m\n",
    "  \u{1b}[90m    Illuminate\\Foundation\\Application::handleCommand(Object(Symfony\\Component\\Console\\Input\\ArgvInput))\u{1b}[39m\n",
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

const C1_THROW_L13_LONG: &str = concat!(
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

const C1_THROW_L13_SHORT_PTY: &str = concat!(
    "\r\n",
    " INFO Running migrations. \r\n",
    "\r\n",
    " 2026_02_01_000001_throw_after_alter_orders_table .. \r\n",
    "\r\n",
    "   RuntimeException \r\n",
    "\r\n",
    "  boom from a migration\r\n",
    "\r\n",
    "  at database/migrations/step10/c1_throw/2026_02_01_000001_throw_after_alter_orders_table.php:15\r\n",
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
    "  28  artisan:16\r\n",
    "      Illuminate\\Foundation\\Application::handleCommand(Object(Symfony\\Component\\Console\\Input\\ArgvInput))\r\n",
    "\r\n",
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

const C1_THROW_L13_SYMFONY: &str = concat!(
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

const C2_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000002_php_error_in_orders_table ................................  \n",
    "\n",
    "   BadMethodCallException \n",
    "\n",
    "  Method Illuminate\\Database\\Schema\\Blueprint::notAMethod does not exist.\n",
    "\n",
    "  at vendor/laravel/framework/src/Illuminate/Macroable/Traits/Macroable.php:117\n",
    "    113▕      */\n",
    "    114▕     public function __call($method, $parameters)\n",
    "    115▕     {\n",
    "    116▕         if (! static::hasMacro($method)) {\n",
    "  ➜ 117▕             throw new BadMethodCallException(sprintf(\n",
    "    118▕                 'Method %s::%s does not exist.', static::class, $method\n",
    "    119▕             ));\n",
    "    120▕         }\n",
    "    121▕\n",
    "\n",
    "  1   database/migrations/step10/c2_php_error/2026_02_01_000002_php_error_in_orders_table.php:14\n",
    "      Illuminate\\Database\\Schema\\Blueprint::__call(\"notAMethod\", [])\n",
    "      \u{1b}[2m+9 vendor frames \u{1b}[22m\n",
    "\n",
    "  11  database/migrations/step10/c2_php_error/2026_02_01_000002_php_error_in_orders_table.php:12\n",
    "      Illuminate\\Support\\Facades\\Facade::__callStatic(\"table\")\n",
    "\n",
);

const C2_L13_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_02_01_000002_php_error_in_orders_table .. \n",
    "\n",
    "   BadMethodCallException \n",
    "\n",
    "  Method Illuminate\\Database\\Schema\\Blueprint::notAMethod does not exist.\n",
    "\n",
    "  at vendor/laravel/framework/src/Illuminate/Macroable/Traits/Macroable.php:117\n",
    "    113▕      */\n",
    "    114▕     public function __call($method, $parameters)\n",
    "    115▕     {\n",
    "    116▕         if (! static::hasMacro($method)) {\n",
    "  ➜ 117▕             throw new BadMethodCallException(sprintf(\n",
    "    118▕                 'Method %s::%s does not exist.', static::class, $method\n",
    "    119▕             ));\n",
    "    120▕         }\n",
    "    121▕\n",
    "\n",
    "  1   database/migrations/step10/c2_php_error/2026_02_01_000002_php_error_in_orders_table.php:14\n",
    "      Illuminate\\Database\\Schema\\Blueprint::__call(\"notAMethod\", [])\n",
    "      \u{1b}[2m+9 vendor frames \u{1b}[22m\n",
    "\n",
    "  11  database/migrations/step10/c2_php_error/2026_02_01_000002_php_error_in_orders_table.php:12\n",
    "      Illuminate\\Support\\Facades\\Facade::__callStatic(\"table\")\n",
    "\n",
);

const C2_L12_SYMFONY: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000002_php_error_in_orders_table ................................  \n",
    "\n",
    "In Macroable.php line 117:\n",
    "                                                                           \n",
    "  Method Illuminate\\Database\\Schema\\Blueprint::notAMethod does not exist.  \n",
    "                                                                           \n",
    "\n",
);

const C2_L13_SYMFONY: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000002_php_error_in_orders_table ................................  \n",
    "\n",
    "In Macroable.php line 117:\n",
    "                                                                           \n",
    "  Method Illuminate\\Database\\Schema\\Blueprint::notAMethod does not exist.  \n",
    "                                                                           \n",
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

const C3_L13_SHORT: &str = concat!(
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

const C3B_L12: &str = concat!(
    "\n",
    "   ParseError \n",
    "\n",
    "  syntax error, unexpected token \"}\"\n",
    "\n",
    "  at database/migrations/step10/c3b_syntax_error_in_2nd/2026_02_01_000032_syntax_error_in_orders_table.php:13\n",
    "      9▕     public function up(): void\n",
    "     10▕     {\n",
    "     11▕         Schema::table('orders', function (Blueprint $table) {\n",
    "     12▕             $table->integer('qty2')->nullable()\n",
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

const C4_L12_ANSI: &str = concat!(
    "\n",
    "  \u{1b}[37;44m INFO \u{1b}[39;49m Running migrations.  \n",
    "\n",
    "  2026_02_01_000041_first_alter_orders_table \u{1b}[90m.................................\u{1b}[39m  \n",
    "  \u{1b}[90m⇂ alter table `orders` add `qty` int null\u{1b}[39m  \n",
    "  2026_02_01_000042_second_throws_orders_table \u{1b}[90m...............................\u{1b}[39m  \n",
    "\n",
    "  \u{1b}[41;1m RuntimeException \u{1b}[49;22m\n",
    "\n",
    "\u{1b}[39;1m  boom from a migration\u{1b}[39;22m\n",
    "\n",
    "  at \u{1b}[32mdatabase/migrations/step10/c4_three_error_in_2nd/2026_02_01_000042_second_throws_orders_table.php\u{1b}[39m:\u{1b}[32m15\u{1b}[39m\n",
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
    "  \u{1b}[33m28  \u{1b}[39m\u{1b}[39;1martisan\u{1b}[39;22m:\u{1b}[39;1m16\u{1b}[39;22m\n",
    "  \u{1b}[90m    Illuminate\\Foundation\\Application::handleCommand(Object(Symfony\\Component\\Console\\Input\\ArgvInput))\u{1b}[39m\n",
    "\n",
);

const C4_L12_PTY_COLS60: &str = concat!(
    "\r\n",
    "  \u{1b}[37;44m INFO \u{1b}[39;49m Running migrations.  \r\n",
    "\r\n",
    "  2026_02_01_000041_first_alter_orders_table \u{1b}[90m.............\u{1b}[39m  \r\n",
    "  \u{1b}[90m⇂ alter table `orders` add `qty` int null\u{1b}[39m  \r\n",
    "  2026_02_01_000042_second_throws_orders_table \u{1b}[90m...........\u{1b}[39m  \r\n",
    "\r\n",
    "  \u{1b}[41;1m RuntimeException \u{1b}[49;22m\r\n",
    "\r\n",
    "\u{1b}[39;1m  boom from a migration\u{1b}[39;22m\r\n",
    "\r\n",
    "  at \u{1b}[32mdatabase/migrations/step10/c4_three_error_in_2nd/2026_02_01_000042_second_throws_orders_table.php\u{1b}[39m:\u{1b}[32m15\u{1b}[39m\r\n",
    "    \u{1b}[90m 11\u{1b}[0m\u{1b}[90m▕ \u{1b}[0m\u{1b}[39;1m    \u{1b}[0m\u{1b}[35;1m{\u{1b}[0m\r\n",
    "    \u{1b}[90m 12\u{1b}[0m\u{1b}[90m▕ \u{1b}[0m\u{1b}[35;1m        \u{1b}[0m\u{1b}[39;1mSchema\u{1b}[0m\u{1b}[35;1m::\u{1b}[0m\u{1b}[39;1mtable\u{1b}[0m\u{1b}[35;1m(\u{1b}[0m\u{1b}[37m'orders'\u{1b}[0m\u{1b}[35;1m, function (\u{1b}[0m\u{1b}[39;1mBlueprint $table\u{1b}[0m\u{1b}[35;1m) {\u{1b}[0m\r\n",
    "    \u{1b}[90m 13\u{1b}[0m\u{1b}[90m▕ \u{1b}[0m\u{1b}[35;1m            \u{1b}[0m\u{1b}[39;1m$table\u{1b}[0m\u{1b}[35;1m->\u{1b}[0m\u{1b}[39;1minteger\u{1b}[0m\u{1b}[35;1m(\u{1b}[0m\u{1b}[37m'qty2'\u{1b}[0m\u{1b}[35;1m)->\u{1b}[0m\u{1b}[39;1mnullable\u{1b}[0m\u{1b}[35;1m();\u{1b}[0m\r\n",
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

const C4_L12_COLS40: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000041_first_alter_orders_table   \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
    "  2026_02_01_000042_second_throws_orders_table   \n",
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

const C4_L12_COLS48: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000041_first_alter_orders_table .  \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
    "  2026_02_01_000042_second_throws_orders_table   \n",
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

const C4_L12_COLS50: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000041_first_alter_orders_table ...  \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
    "  2026_02_01_000042_second_throws_orders_table .  \n",
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

const C4_L13_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_02_01_000041_first_alter_orders_table .. \n",
    " ⇂ alter table `orders` add `qty` int null \n",
    " 2026_02_01_000042_second_throws_orders_table .. \n",
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

const C4_L13_SYMFONY: &str = concat!(
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

const C5A_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000051_guard_has_table_orders_table .............................  \n",
    "  ⇂ select exists (select 1 from information_schema.tables where table_schema = schema() and table_name = 'orders' and table_type in ('BASE TABLE', 'SYSTEM VERSIONED')) as `exists`  \n",
    "\n",
);

const C5A_L12_PTY: &str = concat!(
    "\r\n",
    "  \u{1b}[37;44m INFO \u{1b}[39;49m Running migrations.  \r\n",
    "\r\n",
    "  2026_02_01_000051_guard_has_table_orders_table \u{1b}[90m.............................\u{1b}[39m  \r\n",
    "  \u{1b}[90m⇂ select exists (select 1 from information_schema.tables where table_schema = schema() and table_name = 'orders' and table_type in ('BASE TABLE', 'SYSTEM VERSIONED')) as `exists`\u{1b}[39m  \r\n",
    "\r\n",
);

const C5A_L13_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_02_01_000051_guard_has_table_orders_table .. \n",
    " ⇂ select exists (select 1 from information_schema.tables where table_schema = schema() and table_name = 'orders' and table_type in ('BASE TABLE', 'SYSTEM VERSIONED')) as `exists` \n",
    "\n",
);

const C5B_L13_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_02_01_000052_guard_has_column_orders_table .. \n",
    " ⇂ select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as `collation`, is_nullable as `nullable`, column_default as `default`, column_comment as `comment`, generation_expression as `expression`, extra as `extra` from information_schema.columns where table_schema = schema() and table_name = 'orders' order by ordinal_position asc \n",
    "\n",
);

const C5B_L12_COLS40: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000052_guard_has_column_orders_table   \n",
    "  ⇂ select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as `collation`, is_nullable as `nullable`, column_default as `default`, column_comment as `comment`, generation_expression as `expression`, extra as `extra` from information_schema.columns where table_schema = schema() and table_name = 'orders' order by ordinal_position asc  \n",
    "\n",
);

const C5C_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000053_guard_exists_orders_table ................................  \n",
    "  ⇂ select exists(select * from `orders`) as `exists`  \n",
    "\n",
);

const C5D_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000054_guard_select_orders_table ................................  \n",
    "  ⇂ select id from orders where amount = 5  \n",
    "\n",
);

const C5E_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000055_guard_not_has_column_orders_table ........................  \n",
    "  ⇂ select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as `collation`, is_nullable as `nullable`, column_default as `default`, column_comment as `comment`, generation_expression as `expression`, extra as `extra` from information_schema.columns where table_schema = schema() and table_name = 'orders' order by ordinal_position asc  \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
    "\n",
);

const C5F_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000056_guard_not_has_table_orders_table .........................  \n",
    "  ⇂ select exists (select 1 from information_schema.tables where table_schema = schema() and table_name = 'orders' and table_type in ('BASE TABLE', 'SYSTEM VERSIONED')) as `exists`  \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
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

const C6_L13_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000006_unguarded_read_orders_table ..............................  \n",
    "  ⇂ select count(*) as `aggregate` from `orders`  \n",
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

const C7_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000007_add_a_very_long_descriptive_name_for_the_note_column_to_the_orders_table   \n",
    "  ⇂ alter table `orders` add `a_rather_long_column_name_used_for_wrapping_tests` varchar(100) null  \n",
    "  ⇂ alter table `orders` add index `orders_amount_email_long_index_name_for_wrapping`(`amount`, `email`)  \n",
    "\n",
);

const C7_L12_COLS40_PTY: &str = concat!(
    "\r\n",
    "  \u{1b}[37;44m INFO \u{1b}[39;49m Running migrations.  \r\n",
    "\r\n",
    "  2026_02_01_000007_add_a_very_long_descriptive_name_for_the_note_column_to_the_orders_table   \r\n",
    "  \u{1b}[90m⇂ alter table `orders` add `a_rather_long_column_name_used_for_wrapping_tests` varchar(100) null\u{1b}[39m  \r\n",
    "  \u{1b}[90m⇂ alter table `orders` add index `orders_amount_email_long_index_name_for_wrapping`(`amount`, `email`)\u{1b}[39m  \r\n",
    "\r\n",
);

const C9_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  AddQtyToOrdersTable ........................................................  \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
    "\n",
);

const C9_L12_COLS40: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  AddQtyToOrdersTable ................  \n",
    "  ⇂ alter table `orders` add `qty` int null  \n",
    "\n",
);

const C9_L12_COLS60_PTY: &str = concat!(
    "\r\n",
    "  \u{1b}[37;44m INFO \u{1b}[39;49m Running migrations.  \r\n",
    "\r\n",
    "  AddQtyToOrdersTable \u{1b}[90m....................................\u{1b}[39m  \r\n",
    "  \u{1b}[90m⇂ alter table `orders` add `qty` int null\u{1b}[39m  \r\n",
    "\r\n",
);

const C10_L12: &str = concat!("\n", "   INFO  Nothing to migrate.  \n", "\n",);

const C10_L13_SHORT: &str = concat!("\n", " INFO Nothing to migrate. \n", "\n",);

const DERIVED_L10_C5A: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000051_guard_has_table_orders_table .............................  \n",
    "  ⇂ select table_name as `name`, (data_length + index_length) as `size`, table_comment as `comment`, engine as `engine`, table_collation as `collation` from information_schema.tables where table_schema = 'kilit_laravel_10' and table_type in ('BASE TABLE', 'SYSTEM VERSIONED') order by table_name  \n",
    "\n",
);

const DERIVED_L10_C5B: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000052_guard_has_column_orders_table ............................  \n",
    "  ⇂ select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as `collation`, is_nullable as `nullable`, column_default as `default`, column_comment as `comment`, extra as `extra` from information_schema.columns where table_schema = 'kilit_laravel_10' and table_name = 'orders' order by ordinal_position asc  \n",
    "\n",
);

const DERIVED_L11_C5A: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000051_guard_has_table_orders_table .............................  \n",
    "  ⇂ select table_name as `name`, (data_length + index_length) as `size`, table_comment as `comment`, engine as `engine`, table_collation as `collation` from information_schema.tables where table_schema = 'kilit_laravel_11' and table_type in ('BASE TABLE', 'SYSTEM VERSIONED') order by table_name  \n",
    "\n",
);

const DERIVED_L11_C5B: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_02_01_000052_guard_has_column_orders_table ............................  \n",
    "  ⇂ select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as `collation`, is_nullable as `nullable`, column_default as `default`, column_comment as `comment`, generation_expression as `expression`, extra as `extra` from information_schema.columns where table_schema = 'kilit_laravel_11' and table_name = 'orders' order by ordinal_position asc  \n",
    "\n",
);

use kilit_core::{LaravelMigration, PRETEND_READ_NOTE, Pretend};

const THROW_NAME: &str = "2026_02_01_000001_throw_after_alter_orders_table";
const PHP_ERROR_NAME: &str = "2026_02_01_000002_php_error_in_orders_table";
const SYNTAX_ERROR_NAME: &str = "2026_02_01_000003_syntax_error_in_orders_table";
const SYNTAX_ERROR_2ND_NAME: &str = "2026_02_01_000032_syntax_error_in_orders_table";
const FIRST_NAME: &str = "2026_02_01_000041_first_alter_orders_table";
const SECOND_NAME: &str = "2026_02_01_000042_second_throws_orders_table";
const ADD_QTY: &str = "alter table `orders` add `qty` int null";
const BEFORE_ANY_MIGRATION: &str =
    "Laravel stopped with an error before it printed any migration; no statement was seen";

fn stopped_in(name: &str) -> String {
    format!(
        "Laravel stopped with an error in {name}; its statements and the migrations after it \
         were not seen"
    )
}

fn stopped_after(name: &str) -> String {
    format!("Laravel stopped with an error after {name}; the migrations after it were not seen")
}

fn stopped_in_before_any_migration(name: &str) -> String {
    format!(
        "Laravel stopped with an error in {name} before it printed any migration; no statement \
         was seen"
    )
}

/// The output as Laravel output: an output that is only an error screen is
/// still Laravel's, not SQL.
fn pretend_of(output: &str) -> Pretend {
    read_pretend(output).expect("Laravel's output")
}

fn migration_names(pretend: &Pretend) -> Vec<&str> {
    pretend
        .migrations
        .iter()
        .map(|migration| migration.name.as_str())
        .collect()
}

fn sql_of(migration: &LaravelMigration) -> Vec<&str> {
    migration
        .statements
        .iter()
        .map(|statement| statement.sql.as_str())
        .collect()
}

/// `output` without the lines that start with `prefix`.
fn without_lines_starting_with(output: &str, prefix: &str) -> String {
    output
        .split_inclusive('\n')
        .filter(|line| !line.trim_start().starts_with(prefix))
        .collect()
}

/// Every output of a migration that throws, with what each form must give.
/// Laravel prints a migration's name before it runs it and its statements
/// after, so the migration that throws is the last name and has none.
#[test]
fn a_migration_that_throws_is_the_one_laravel_stopped_in_in_every_form_of_the_screen() {
    // Schema::table('orders', ...) with $table->integer('qty')->nullable(),
    // then throw new RuntimeException('boom from a migration').
    for (form, output) in [
        ("Laravel 12, Collision, piped", C1_THROW_L12),
        ("Laravel 12, Collision, --ansi", C1_THROW_L12_ANSI),
        ("Laravel 12, Collision, terminal", C1_THROW_L12_PTY),
        ("Laravel 13, Collision, long form", C1_THROW_L13_LONG),
        ("Laravel 13, Collision, short form", C1_THROW_L13_SHORT),
        (
            "Laravel 13, Collision, short form, terminal",
            C1_THROW_L13_SHORT_PTY,
        ),
        ("Laravel 12, Symfony", C1_THROW_L12_SYMFONY),
        ("Laravel 13, Symfony, --no-ansi", C1_THROW_L13_SYMFONY),
    ] {
        let pretend = pretend_of(output);
        assert!(
            pretend.migrations.is_empty(),
            "{form}: {:?}",
            pretend.migrations
        );
        assert!(
            pretend.unreadable.is_empty(),
            "{form}: {:?}",
            pretend.unreadable
        );
        let error = pretend.error.unwrap_or_else(|| panic!("{form}: no error"));
        assert_eq!(error.migration.as_deref(), Some(THROW_NAME), "{form}");
        assert_eq!(error.message, stopped_in(THROW_NAME), "{form}");
        // The screen's first line: the exception class, or Symfony's "In".
        assert_eq!(error.line, 6, "{form}");
    }
}

#[test]
fn an_error_thrown_in_a_vendor_file_is_still_attributed_to_the_migration_from_its_name_line() {
    // $table->notAMethod() on a Blueprint: BadMethodCallException from
    // vendor/.../Macroable.php. Symfony's screen names only "Macroable.php".
    for (form, output) in [
        ("Laravel 12, Collision", C2_L12),
        ("Laravel 13, Collision, short form", C2_L13_SHORT),
        ("Laravel 12, Symfony", C2_L12_SYMFONY),
        ("Laravel 13, Symfony", C2_L13_SYMFONY),
    ] {
        let pretend = pretend_of(output);
        assert!(pretend.migrations.is_empty(), "{form}");
        assert!(
            pretend.unreadable.is_empty(),
            "{form}: {:?}",
            pretend.unreadable
        );
        let error = pretend.error.unwrap_or_else(|| panic!("{form}: no error"));
        assert_eq!(error.migration.as_deref(), Some(PHP_ERROR_NAME), "{form}");
        assert_eq!(error.message, stopped_in(PHP_ERROR_NAME), "{form}");
        assert_eq!(error.line, 6, "{form}");
    }
}

#[test]
fn a_parse_error_is_laravel_output_and_names_the_file_before_any_migration() {
    // A missing semicolon in the only migration: ParseError, and Laravel
    // prints no INFO line and no migration name.
    for (form, output, name, line) in [
        ("Laravel 12, Collision", C3_L12, SYNTAX_ERROR_NAME, 2),
        (
            "Laravel 13, Collision, short form",
            C3_L13_SHORT,
            SYNTAX_ERROR_NAME,
            2,
        ),
        ("Laravel 12, Symfony", C3_L12_SYMFONY, SYNTAX_ERROR_NAME, 2),
        // The syntax error is in the 2nd of 3 migrations; Laravel still
        // prints nothing before the screen.
        ("2nd of 3, Collision", C3B_L12, SYNTAX_ERROR_2ND_NAME, 2),
    ] {
        let pretend = pretend_of(output);
        assert!(pretend.migrations.is_empty(), "{form}");
        assert!(
            pretend.unreadable.is_empty(),
            "{form}: {:?}",
            pretend.unreadable
        );
        let error = pretend.error.unwrap_or_else(|| panic!("{form}: no error"));
        assert_eq!(error.migration.as_deref(), Some(name), "{form}");
        assert_eq!(
            error.message,
            stopped_in_before_any_migration(name),
            "{form}"
        );
        assert_eq!(error.line, line, "{form}");
    }
}

#[test]
fn an_error_in_the_second_migration_keeps_the_first_and_names_the_second() {
    // Three migrations: an ALTER, one that throws, an ALTER. Only the first
    // is printed. The runs at 40 columns have no dots on a name line, those
    // at 48 and 50 have one to three.
    for (form, output) in [
        ("Laravel 12, Collision, piped", C4_L12),
        ("Laravel 12, Collision, --ansi", C4_L12_ANSI),
        (
            "Laravel 12, Collision, terminal, 60 columns",
            C4_L12_PTY_COLS60,
        ),
        ("Laravel 12, Collision, 40 columns", C4_L12_COLS40),
        ("Laravel 12, Collision, 48 columns", C4_L12_COLS48),
        ("Laravel 12, Collision, 50 columns", C4_L12_COLS50),
        ("Laravel 13, Collision, short form", C4_L13_SHORT),
        ("Laravel 12, Symfony", C4_L12_SYMFONY),
        ("Laravel 13, Symfony", C4_L13_SYMFONY),
    ] {
        let pretend = pretend_of(output);
        assert!(
            pretend.unreadable.is_empty(),
            "{form}: {:?}",
            pretend.unreadable
        );
        assert_eq!(migration_names(&pretend), [FIRST_NAME], "{form}");
        assert_eq!(sql_of(&pretend.migrations[0]), [ADD_QTY], "{form}");
        assert_eq!(pretend.migrations[0].statements[0].line, 5, "{form}");
        let error = pretend.error.unwrap_or_else(|| panic!("{form}: no error"));
        assert_eq!(error.migration.as_deref(), Some(SECOND_NAME), "{form}");
        assert_eq!(error.message, stopped_in(SECOND_NAME), "{form}");
        assert_eq!(error.line, 8, "{form}");
    }
}

#[test]
fn the_statements_printed_before_the_error_keep_their_answers() {
    // ADD COLUMN `qty` int null on a table that has no `qty`: INSTANT.
    for server in ["mysql-8.0.46", "mysql-8.4.13"] {
        for output in [C4_L12, C4_L12_SYMFONY, C4_L13_SHORT] {
            assert_eq!(answers(server, ORDERS, output), [INSTANT], "{server}");
        }
    }
}

#[test]
fn an_error_screen_after_a_migration_that_printed_statements_names_the_migration_before_it() {
    // Edited output: the 2nd migration's name line is cut from the output
    // above, so the screen follows the first migration's statement.
    for (form, output) in [
        ("Collision", C4_L12),
        ("Collision, short form", C4_L13_SHORT),
        ("Symfony", C4_L12_SYMFONY),
    ] {
        let output = without_lines_starting_with(output, "2026_02_01_000042");
        let pretend = pretend_of(&output);
        assert_eq!(migration_names(&pretend), [FIRST_NAME], "{form}");
        assert_eq!(sql_of(&pretend.migrations[0]), [ADD_QTY], "{form}");
        let error = pretend.error.unwrap_or_else(|| panic!("{form}: no error"));
        assert_eq!(error.migration, None, "{form}");
        assert_eq!(error.message, stopped_after(FIRST_NAME), "{form}");
        assert_eq!(error.line, 7, "{form}");
    }
}

const UNIX_THROW_FILE: &str = "at database/migrations/step10/c1_throw/2026_02_01_000001_throw_after_alter_orders_table.php:15";
const WINDOWS_THROW_FILE: &str =
    "at C:\\app\\database\\migrations\\2026_02_01_000001_throw_after_alter_orders_table.php:15";

#[test]
fn an_error_screen_with_no_migration_name_names_the_file_only_if_it_is_a_migration() {
    // Edited outputs: the migration's name line is cut, so Laravel appears to
    // have printed no migration before the screen.
    for (form, output) in [
        (
            "Collision",
            without_lines_starting_with(C1_THROW_L12, "2026_02_01_000001"),
        ),
        (
            "Symfony",
            without_lines_starting_with(C1_THROW_L12_SYMFONY, "2026_02_01_000001"),
        ),
        // Windows: the file in the screen has backslashes.
        (
            "Collision, Windows path",
            without_lines_starting_with(C1_THROW_L12, "2026_02_01_000001")
                .replace(UNIX_THROW_FILE, WINDOWS_THROW_FILE),
        ),
    ] {
        let pretend = pretend_of(&output);
        assert!(pretend.migrations.is_empty(), "{form}");
        let error = pretend.error.unwrap_or_else(|| panic!("{form}: no error"));
        assert_eq!(error.migration.as_deref(), Some(THROW_NAME), "{form}");
        assert_eq!(
            error.message,
            stopped_in_before_any_migration(THROW_NAME),
            "{form}"
        );
    }

    // The same with the vendor file of a BadMethodCallException: Macroable
    // is no migration.
    for (form, output) in [
        (
            "Collision",
            without_lines_starting_with(C2_L12, "2026_02_01_000002"),
        ),
        (
            "Symfony",
            without_lines_starting_with(C2_L12_SYMFONY, "2026_02_01_000002"),
        ),
    ] {
        let pretend = pretend_of(&output);
        assert!(pretend.migrations.is_empty(), "{form}");
        let error = pretend.error.unwrap_or_else(|| panic!("{form}: no error"));
        assert_eq!(error.migration, None, "{form}");
        assert_eq!(error.message, BEFORE_ANY_MIGRATION, "{form}");
    }
}

#[test]
fn nothing_on_the_error_screen_becomes_a_migration_a_statement_or_an_unreadable_line() {
    // RuntimeException, "11▕", "➜  15▕ throw new ...", "+27 vendor frames",
    // "Illuminate\Foundation\Application::handleCommand(...)", "artisan:16",
    // "In ... line 15:" and the message are all on the screen.
    for output in [
        C1_THROW_L12,
        C1_THROW_L12_ANSI,
        C1_THROW_L12_PTY,
        C1_THROW_L13_LONG,
        C1_THROW_L13_SHORT,
        C1_THROW_L13_SHORT_PTY,
        C1_THROW_L12_SYMFONY,
        C1_THROW_L13_SYMFONY,
        C2_L12,
        C2_L13_SHORT,
        C2_L12_SYMFONY,
        C2_L13_SYMFONY,
        C3_L12,
        C3_L13_SHORT,
        C3_L12_SYMFONY,
        C3B_L12,
        C4_L12,
        C4_L12_ANSI,
        C4_L12_PTY_COLS60,
        C4_L12_COLS40,
        C4_L13_SHORT,
        C4_L12_SYMFONY,
        C4_L13_SYMFONY,
    ] {
        let pretend = pretend_of(output);
        assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
        for migration in &pretend.migrations {
            assert!(
                migration.name.starts_with("2026_02_01_00004"),
                "{}",
                migration.name
            );
            assert_eq!(sql_of(migration), [ADD_QTY]);
        }
        assert!(pretend.error.is_some());
    }
}

#[test]
fn a_migration_that_read_before_the_error_keeps_its_first_read() {
    // Edited output: the first migration of the run above reads the
    // database before its ALTER.
    let output = C4_L12.replacen(
        "  \u{21c2} alter table `orders` add `qty` int null  \n",
        "  \u{21c2} select count(*) as aggregate from `orders`  \n  \u{21c2} alter table `orders` add `qty` int null  \n",
        1,
    );
    let pretend = pretend_of(&output);
    assert_eq!(migration_names(&pretend), [FIRST_NAME]);
    assert_eq!(pretend.migrations[0].first_read, Some(0));
    assert_eq!(pretend.migrations[0].statements.len(), 2);
    assert_eq!(
        pretend.error.unwrap().migration.as_deref(),
        Some(SECOND_NAME)
    );
}

// Laravel prints a name line with dots, or, where the terminal is too narrow
// for them (a long name, a small COLUMNS), without.

#[test]
fn a_name_line_without_dots_is_a_migration_if_it_has_the_migration_file_pattern() {
    // A 90-character name does not fit 80 columns: no dots at 80, 60 and 40.
    let name = "2026_02_01_000007_add_a_very_long_descriptive_name_for_the_note_column_to_the_orders_table";
    for (form, output) in [
        ("Laravel 12, 80 columns", C7_L12),
        ("Laravel 12, 40 columns, terminal", C7_L12_COLS40_PTY),
    ] {
        let pretend = pretend_of(output);
        assert!(
            pretend.unreadable.is_empty(),
            "{form}: {:?}",
            pretend.unreadable
        );
        assert_eq!(migration_names(&pretend), [name], "{form}");
        assert_eq!(pretend.migrations[0].statements.len(), 2, "{form}");
        assert_eq!(pretend.migrations[0].first_read, None, "{form}");
        assert!(pretend.error.is_none(), "{form}");
    }
}

#[test]
fn a_line_without_dots_that_is_not_a_migration_file_name_is_unreadable() {
    for text in [
        "RuntimeException",
        "AddQtyToOrdersTable",
        "add_qty_to_orders_table",
        // year, month, day or time with the wrong number of digits
        "26_02_01_000007_add_qty",
        "2026_2_01_000007_add_qty",
        "2026_02_1_000007_add_qty",
        "2026_02_01_00007_add_qty",
        "2026_02_01_0000007_add_qty",
        // letters where digits belong
        "2026_02_01_00000a_add_qty",
    ] {
        let output = format!("\n   INFO  Running migrations.  \n\n  {text}  \n\n");
        let pretend = pretend_of(&output);
        assert!(
            pretend.migrations.is_empty(),
            "{text}: {:?}",
            pretend.migrations
        );
        assert!(pretend.error.is_none(), "{text}");
        let unreadable: Vec<_> = pretend
            .unreadable
            .iter()
            .map(|line| (line.line, line.text.as_str()))
            .collect();
        assert_eq!(unreadable, [(4, text)], "{text}");
    }
}

#[test]
fn a_migration_with_a_class_name_is_read_with_and_without_dots() {
    // class AddQtyToOrdersTable extends Migration, in the file
    // 2026_02_01_000009_add_qty_to_orders_table.php: Laravel prints the class.
    for (form, output) in [
        ("80 columns", C9_L12),
        ("40 columns", C9_L12_COLS40),
        ("60 columns, terminal", C9_L12_COLS60_PTY),
    ] {
        let pretend = pretend_of(output);
        assert!(
            pretend.unreadable.is_empty(),
            "{form}: {:?}",
            pretend.unreadable
        );
        assert_eq!(migration_names(&pretend), ["AddQtyToOrdersTable"], "{form}");
        assert_eq!(sql_of(&pretend.migrations[0]), [ADD_QTY], "{form}");
        assert!(pretend.error.is_none(), "{form}");
    }
}

#[test]
fn nothing_to_migrate_is_laravel_output_with_no_migration_error_or_unreadable_line() {
    for (form, output) in [("long form", C10_L12), ("short form", C10_L13_SHORT)] {
        let pretend = pretend_of(output);
        assert!(pretend.migrations.is_empty(), "{form}");
        assert!(pretend.unreadable.is_empty(), "{form}");
        assert!(pretend.error.is_none(), "{form}");
    }
}

#[test]
fn only_the_arrow_and_its_mangled_forms_mark_a_statement() {
    // Laravel prints a leading arrow; a Windows code page shows its three
    // UTF-8 bytes as other characters; a console that cannot show it prints
    // one or three question marks.
    for mark in [
        "\u{21c2}",
        "?",
        "???",
        "Γçé",
        "Ôçé",
        "РЄѓ",
        "Γחג",
        "ﻗ┼∙",
        "тЗВ",
        "â‡‚",
        "в‡‚",
        "β‡‚",
        "ג‡‚",
        "ā‡‚",
    ] {
        let output = format!(
            "\n   INFO  Running migrations.  \n\n  2026_02_01_000001_add_qty_to_orders_table ....  \n  {mark} {ADD_QTY}  \n\n"
        );
        let pretend = pretend_of(&output);
        assert!(
            pretend.unreadable.is_empty(),
            "{mark}: {:?}",
            pretend.unreadable
        );
        assert_eq!(pretend.migrations.len(), 1, "{mark}");
        assert_eq!(sql_of(&pretend.migrations[0]), [ADD_QTY], "{mark}");
    }
}

#[test]
fn other_leading_words_are_no_statement_mark() {
    // The error screen's pointer, an arrow of another kind, an accented
    // word, a path and a class name.
    for word in [
        "\u{279c}",
        "\u{2192}",
        "ünlü",
        "->",
        "RuntimeException",
        "Illuminate\\Foundation\\Application::handleCommand",
        "??",
        "????",
    ] {
        let output = format!(
            "\n   INFO  Running migrations.  \n\n  2026_02_01_000001_add_qty_to_orders_table ....  \n  {word} {ADD_QTY}  \n\n"
        );
        let pretend = pretend_of(&output);
        assert_eq!(pretend.migrations.len(), 1, "{word}");
        assert!(pretend.migrations[0].statements.is_empty(), "{word}");
        let unreadable: Vec<_> = pretend.unreadable.iter().map(|line| line.line).collect();
        assert_eq!(unreadable, [5], "{word}");
    }
}

// Reads. Under --pretend every read returns nothing, so what a migration
// does after a read is not what it would do on the database.

#[test]
fn a_migration_that_asks_the_database_first_has_its_first_read_at_statement_0() {
    // Schema::hasTable('orders'), Schema::hasColumn('orders', 'note'),
    // DB::table('orders')->exists() and DB::select('select id from orders
    // ...') each guard an ALTER: under --pretend the read returns nothing, so
    // Laravel prints the read and not the ALTER. `! Schema::hasColumn()` and
    // `! Schema::hasTable()` guard it the other way, so the ALTER is printed
    // after the read, and DB::table('orders')->count() is a read with no guard.
    for (what, output, statements) in [
        ("hasTable, Laravel 12", C5A_L12, 1),
        ("hasTable, Laravel 12, terminal", C5A_L12_PTY, 1),
        ("hasTable, Laravel 13, short form", C5A_L13_SHORT, 1),
        ("hasColumn, Laravel 13, short form", C5B_L13_SHORT, 1),
        ("hasColumn, Laravel 12, 40 columns", C5B_L12_COLS40, 1),
        ("exists(), Laravel 12", C5C_L12, 1),
        ("DB::select(), Laravel 12", C5D_L12, 1),
        ("!hasColumn then ALTER, Laravel 12", C5E_L12, 2),
        ("!hasTable then ALTER, Laravel 12", C5F_L12, 2),
        ("count() then ALTER, Laravel 12", C6_L12, 2),
        ("count() then ALTER, Laravel 13", C6_L13_LONG, 2),
    ] {
        let pretend = pretend_of(output);
        assert!(
            pretend.unreadable.is_empty(),
            "{what}: {:?}",
            pretend.unreadable
        );
        assert!(pretend.error.is_none(), "{what}");
        assert_eq!(pretend.migrations.len(), 1, "{what}");
        assert_eq!(pretend.migrations[0].statements.len(), statements, "{what}");
        assert_eq!(pretend.migrations[0].first_read, Some(0), "{what}");
    }
}

#[test]
fn laravel_10_and_11_reads_of_has_table_and_has_column_are_found_too() {
    // Derived from the framework source, not run: Laravel 10 and 11 cannot be
    // installed here (see the note above the constants).
    for (what, output) in [
        ("Laravel 10 hasTable", DERIVED_L10_C5A),
        ("Laravel 10 hasColumn", DERIVED_L10_C5B),
        ("Laravel 11 hasTable", DERIVED_L11_C5A),
        ("Laravel 11 hasColumn", DERIVED_L11_C5B),
    ] {
        let pretend = pretend_of(output);
        assert!(
            pretend.unreadable.is_empty(),
            "{what}: {:?}",
            pretend.unreadable
        );
        assert_eq!(pretend.migrations.len(), 1, "{what}");
        assert_eq!(pretend.migrations[0].statements.len(), 1, "{what}");
        assert_eq!(pretend.migrations[0].first_read, Some(0), "{what}");
    }
}

#[test]
fn many_reads_in_one_migration_give_one_first_read_and_the_alter_comes_last() {
    // select with a binding, selectOne, scalar, first, `show index`,
    // hasIndex, getColumnListing, getTables and DB::statement('select 1'),
    // then the ALTER.
    let pretend = pretend_of(C8_L12);
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    assert_eq!(pretend.migrations.len(), 1);
    let migration = &pretend.migrations[0];
    assert_eq!(migration.statements.len(), 10);
    assert_eq!(migration.first_read, Some(0));
    assert_eq!(migration.statements[9].sql, ADD_QTY);
}

#[test]
fn the_first_read_is_the_first_statement_that_asks_not_the_first_statement() {
    // Edited output: an ALTER, a read, an ALTER; then a migration with no
    // read; then one whose second statement reads.
    let output = concat!(
        "\n   INFO  Running migrations.  \n\n",
        "  2026_02_01_000001_a ....  \n",
        "  \u{21c2} alter table `orders` add `qty` int null  \n",
        "  \u{21c2} select count(*) as aggregate from `orders`  \n",
        "  \u{21c2} alter table `orders` add `qty2` int null  \n",
        "  2026_02_01_000002_b ....  \n",
        "  \u{21c2} alter table `orders` add `qty3` int null  \n",
        "  2026_02_01_000003_c ....  \n",
        "  \u{21c2} alter table `orders` add `qty4` int null  \n",
        "  \u{21c2} select 1  \n",
        "  \u{21c2} select 2  \n",
    );
    let pretend = pretend_of(output);
    let first_reads: Vec<_> = pretend
        .migrations
        .iter()
        .map(|migration| migration.first_read)
        .collect();
    assert_eq!(first_reads, [Some(1), None, Some(1)]);
}

#[test]
fn every_statement_that_starts_a_query_counts_as_a_read_in_any_letter_case() {
    for read in [
        "select 1",
        "SELECT 1",
        "Select id from orders",
        "(select 1)",
        "show index from orders",
        "SHOW TABLES",
        "describe orders",
        "desc orders",
        "explain select 1",
        "with x as (select 1) select * from x",
        "table orders",
        "values row(1, 2)",
    ] {
        let output = format!(
            "\n   INFO  Running migrations.  \n\n  2026_02_01_000001_a ....  \n  \u{21c2} {ADD_QTY}  \n  \u{21c2} {read}  \n"
        );
        let pretend = pretend_of(&output);
        assert!(
            pretend.unreadable.is_empty(),
            "{read}: {:?}",
            pretend.unreadable
        );
        assert_eq!(pretend.migrations[0].statements.len(), 2, "{read}");
        assert_eq!(pretend.migrations[0].first_read, Some(1), "{read}");
    }
}

#[test]
fn statements_that_only_contain_a_read_word_are_no_read() {
    for write in [
        ADD_QTY,
        "alter table `orders` add `selection` int null",
        "alter table `orders` add `show_all` tinyint(1) null",
        "create table `selects` (`id` int not null)",
        "insert into `orders` (`id`) values (1)",
        "update `orders` set `qty` = 1",
        "delete from `orders`",
        "drop table `orders`",
        "rename table `orders` to `old_orders`",
        "set foreign_key_checks = 0",
    ] {
        let output = format!(
            "\n   INFO  Running migrations.  \n\n  2026_02_01_000001_a ....  \n  \u{21c2} {write}  \n"
        );
        let pretend = pretend_of(&output);
        assert_eq!(pretend.migrations[0].first_read, None, "{write}");
    }
    for output in [C1_THROW_L12, C4_L12, C7_L12, C9_L12, C10_L12] {
        for migration in &pretend_of(output).migrations {
            assert_eq!(migration.first_read, None, "{}", migration.name);
        }
    }
}

#[test]
fn a_read_changes_the_answer_of_no_statement() {
    // The ALTER after the read gets the answer it gets without the read.
    let alone = "\n   INFO  Running migrations.  \n\n  2026_02_01_000006_unguarded_read_orders_table ....  \n  \u{21c2} alter table `orders` add `qty` int null  \n";
    for server in ["mysql-8.0.46", "mysql-8.4.13"] {
        let expected = answers(server, ORDERS, alone);
        assert_eq!(expected, [INSTANT], "{server}");
        for output in [C5E_L12, C5F_L12, C6_L12, C6_L13_LONG] {
            let all = answers(server, ORDERS, output);
            assert_eq!(all.len(), 2, "{server}");
            assert_eq!(all[1], expected[0], "{server}");
        }
        let all = answers(server, ORDERS, C8_L12);
        assert_eq!(all.len(), 10, "{server}");
        assert_eq!(all[9], expected[0], "{server}");
    }
}

#[test]
fn the_read_note_says_the_read_returns_nothing_under_pretend() {
    assert_eq!(
        PRETEND_READ_NOTE,
        "this migration reads the database: under --pretend every read returns nothing, so \
         statements that depend on what it reads may be missing here, or may appear although \
         they would not run"
    );
}

#[test]
fn the_oem_code_page_forms_of_the_mark_read_the_statement() {
    // The mark's bytes E2 87 82 as Windows code pages 720, 737 and 775 show
    // them.
    for mark in ["ظçé", "έΘΓ", "Ōćé"] {
        let output = format!(
            "\n   INFO  Running migrations.  \n\n  2026_02_01_000001_add_qty_to_orders_table ....  \n  {mark} {ADD_QTY}  \n\n"
        );
        let pretend = pretend_of(&output);
        assert!(
            pretend.unreadable.is_empty(),
            "{mark}: {:?}",
            pretend.unreadable
        );
        assert!(pretend.error.is_none(), "{mark}");
        assert_eq!(pretend.migrations.len(), 1, "{mark}");
        assert_eq!(sql_of(&pretend.migrations[0]), [ADD_QTY], "{mark}");
    }
}

#[test]
fn sql_with_lines_that_look_like_an_error_screen_is_not_laravel_output() {
    // The "file" of the screen must end in .php. A comment with a bare word
    // between blank lines and a line "at 10:30", or "In plan line 3:", is no
    // error screen.
    for sql in [
        "/*\n\nRollout\n\nat 10:30\n*/\nALTER TABLE orders DROP COLUMN note;\n",
        "/*\n\nnotes\n\nat 20261106:1\n*/\nALTER TABLE orders DROP COLUMN note;\n",
        "/*\n\nRollout\n\nIn plan line 3:\n\n*/\nALTER TABLE orders DROP COLUMN note;\n",
    ] {
        assert!(read_pretend(sql).is_none(), "{sql}");
    }
}

// Step 11a: what Laravel 12 and 13 print for reads and for a create followed
// by a key, and what kilit-core still says about them. Leaving notes out of
// the text is the command's job; the findings keep every note.

fn long_form(migrations: &[(&str, &[&str])]) -> String {
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

fn short_form(migrations: &[(&str, &[&str])]) -> String {
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

const SELECT_HAS_TABLE: &str = "select exists (select 1 from information_schema.tables where table_schema = schema() and table_name = 'users' and table_type in ('BASE TABLE', 'SYSTEM VERSIONED')) as `exists`";
const SELECT_COLUMNS: &str = "select column_name as `name`, data_type as `type_name`, column_type as `type`, collation_name as `collation`, is_nullable as `nullable`, column_default as `default`, column_comment as `comment`, generation_expression as `expression`, extra as `extra` from information_schema.columns where table_schema = schema() and table_name = 'users' order by ordinal_position asc";
const SELECT_EXISTS_USERS: &str = "select exists(select * from `users`) as `exists`";
const ADD_NICKNAME: &str = "alter table `users` add `nickname` varchar(255) null";
const ADD_PHONE: &str = "alter table `users` add `phone` varchar(255) null";

const READS_ONE: &str = "2026_03_01_000001_reads_one_users_table";
const READS_TWO: &str = "2026_03_01_000002_reads_two_users_table";

// Schema::hasTable('users'), Schema::getColumnListing('users') twice,
// DB::table('users')->exists(), Schema::table('users', add nickname),
// Schema::getColumnListing('users'); then a second migration with
// Schema::hasTable, Schema::getColumnListing and Schema::table(add phone).
fn reads_in_both_forms() -> [(&'static str, String); 2] {
    let ones: &[&str] = &[
        SELECT_HAS_TABLE,
        SELECT_COLUMNS,
        SELECT_COLUMNS,
        SELECT_EXISTS_USERS,
        ADD_NICKNAME,
        SELECT_COLUMNS,
    ];
    let twos: &[&str] = &[SELECT_HAS_TABLE, SELECT_COLUMNS, ADD_PHONE];
    [
        ("long", long_form(&[(READS_ONE, ones), (READS_TWO, twos)])),
        ("short", short_form(&[(READS_ONE, ones), (READS_TWO, twos)])),
    ]
}

/// The findings for all statements of the output, with their notes.
fn notes_by_statement(server: &str, schema: &str, output: &str) -> Vec<Vec<String>> {
    let pretend = read_pretend(output).expect("the output of migrate --pretend");
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    let statements: Vec<_> = pretend
        .migrations
        .into_iter()
        .flat_map(|migration| migration.statements)
        .collect();
    let results = Knowledge::built_in().server(server).unwrap();
    check(&statements, &Schema::from_sql(schema), results)
        .into_iter()
        .map(|finding| match finding.prediction {
            Prediction::Measured { notes, .. } => notes,
            _ => Vec::new(),
        })
        .collect()
}

#[test]
fn the_reads_of_laravel_12_and_13_stay_statements_with_one_first_read_per_migration() {
    for (form, output) in reads_in_both_forms() {
        let pretend = pretend_of(&output);
        assert!(pretend.unreadable.is_empty(), "{form}");
        assert_eq!(migration_names(&pretend), [READS_ONE, READS_TWO], "{form}");
        assert_eq!(
            sql_of(&pretend.migrations[0]),
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
        assert_eq!(
            sql_of(&pretend.migrations[1]),
            [SELECT_HAS_TABLE, SELECT_COLUMNS, ADD_PHONE],
            "{form}"
        );
        // Each migration's first read is its first statement.
        assert_eq!(pretend.migrations[0].first_read, Some(0), "{form}");
        assert_eq!(pretend.migrations[1].first_read, Some(0), "{form}");
    }
}

#[test]
fn the_reads_get_not_checked_and_the_alters_after_them_keep_their_answers() {
    for (form, output) in reads_in_both_forms() {
        for server in ["mysql-8.0.46", "mysql-8.4.13"] {
            assert_eq!(
                answers(server, USERS, &output),
                [
                    NOT_CHECKED,
                    NOT_CHECKED,
                    NOT_CHECKED,
                    NOT_CHECKED,
                    INSTANT,
                    NOT_CHECKED,
                    NOT_CHECKED,
                    NOT_CHECKED,
                    INSTANT
                ],
                "{form} on {server}"
            );
        }
    }
}

#[test]
fn the_findings_of_an_alter_after_reads_still_carry_the_64_changes_note() {
    for (form, output) in reads_in_both_forms() {
        let notes = notes_by_statement("mysql-8.4.13", USERS, &output);
        for alter in [4, 8] {
            assert!(
                notes[alter].iter().any(|note| {
                    note.starts_with(
                        "if the table has had 64 instant column changes since it was last \
                         rebuilt: ",
                    )
                }),
                "{form}: {:?}",
                notes[alter]
            );
        }
    }
}

const CREATE_PARENTS: &str = "create table `parents` (`id` bigint unsigned not null auto_increment primary key, `name` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'";
const CREATE_CHILDREN: &str = "create table `children` (`id` bigint unsigned not null auto_increment primary key, `parent_id` bigint unsigned not null, `name` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'";
const INSERT_CHILD: &str = "insert into `children` (`name`, `parent_id`) values ('a', 1)";
const ADD_PARENT_KEY: &str = "alter table `children` add constraint `children_parent_id_foreign` foreign key (`parent_id`) references `parents` (`id`)";

const FIRST_ROW_NOTE: &str = "if a row points to a parent row that does not exist: ";
const CHECKS_ON: &str =
    "assumes foreign_key_checks is ON, the MySQL default: nothing before this statement sets it";

#[test]
fn a_create_and_a_key_with_or_without_an_insert_give_the_same_answer_and_the_same_findings() {
    // Schema::create('parents'), Schema::create('children') with
    // $table->foreignId('parent_id'), [DB::table('children')->insert(...)],
    // Schema::table('children', ... foreign('parent_id')->references('id')
    // ->on('parents')).
    let without: &[&str] = &[CREATE_PARENTS, CREATE_CHILDREN, ADD_PARENT_KEY];
    let with: &[&str] = &[
        CREATE_PARENTS,
        CREATE_CHILDREN,
        INSERT_CHILD,
        ADD_PARENT_KEY,
    ];
    for (form, without, with) in [
        (
            "long",
            long_form(&[("2026_03_02_000002_create_parents_children_fk", without)]),
            long_form(&[("2026_03_02_000001_create_parents_children_insert_fk", with)]),
        ),
        (
            "short",
            short_form(&[("2026_03_02_000002_create_parents_children_fk", without)]),
            short_form(&[("2026_03_02_000001_create_parents_children_insert_fk", with)]),
        ),
    ] {
        for server in ["mysql-8.0.46", "mysql-8.4.13"] {
            assert_eq!(
                answers(server, "", &without),
                [NOT_CHECKED, NOT_CHECKED, COPY],
                "{form} on {server}"
            );
            assert_eq!(
                answers(server, "", &with),
                [NOT_CHECKED, NOT_CHECKED, NOT_CHECKED, COPY],
                "{form} on {server}"
            );
            // The findings keep the row note either way: leaving it out of
            // the text is the command's decision.
            for (output, key) in [(&without, 2), (&with, 3)] {
                let notes = notes_by_statement(server, "", output);
                assert!(
                    notes[key]
                        .iter()
                        .any(|note| note.starts_with(FIRST_ROW_NOTE)),
                    "{form} on {server}: {:?}",
                    notes[key]
                );
                assert_eq!(notes[key][0], CHECKS_ON, "{form} on {server}");
            }
        }
    }
}

#[test]
fn the_insert_between_a_create_and_a_key_is_one_statement_of_the_migration() {
    let with: &[&str] = &[
        CREATE_PARENTS,
        CREATE_CHILDREN,
        INSERT_CHILD,
        ADD_PARENT_KEY,
    ];
    for output in [
        long_form(&[("2026_03_02_000001_create_parents_children_insert_fk", with)]),
        short_form(&[("2026_03_02_000001_create_parents_children_insert_fk", with)]),
    ] {
        let pretend = pretend_of(&output);
        assert_eq!(sql_of(&pretend.migrations[0]), with);
        assert_eq!(pretend.migrations[0].first_read, None);
    }
}
