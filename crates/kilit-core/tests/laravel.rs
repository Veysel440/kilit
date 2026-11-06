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
