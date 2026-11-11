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
            former_names: Vec::new(),
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
fn rename_index_is_read_as_an_alter_of_orders() {
    // $table->renameIndex('a', 'b'); B1c reads RENAME INDEX, so the table it
    // renames an index of stays known.
    assert!(matches!(
        kind("alter table `orders` rename index `a` to `b`"),
        StatementKind::Alter(_)
    ));
    assert_eq!(
        alter("alter table `orders` rename key `a` to `b`")
            .operations
            .len(),
        1
    );
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

use kilit_core::{Knowledge, Prediction, Schema, Shared, Verdict, check, read_pretend};

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
            Prediction::DependsOnTable { shared, .. } => match shared {
                Shared::WritesWait => "depends on the table: writes wait".to_owned(),
                Shared::Fails => "depends on the table: fails".to_owned(),
            },
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

// ---------------------------------------------------------------------------
// Step 11b: `$table->comment('...')`. Laravel 12.71.0 and 13.40.0 print
// `alter table `t` comment = '...'` as a statement of its own, after the
// create table in Schema::create and alone in Schema::table; a quote in the
// comment is doubled. The two versions print the same bytes in a terminal; the
// short form is what Laravel 13.40.0 prints where it detects an automated
// environment. Migrations that only set the engine, the charset or the
// collation of the table print nothing (E).
//
//   a: Schema::create('notes', ... $table->comment('Notes kept by staff'))
//   b: Schema::table('notes', fn ($t) => $t->comment('Notes kept by staff'))
//   c: Schema::table('notes', fn ($t) => $t->comment("It's a note"))
//   d: Schema::table('notes', fn ($t) => { $t->string('extra')->nullable();
//                                          $t->comment('x'); })
//   e: three migrations that set only ->engine, ->charset and ->collation
// ---------------------------------------------------------------------------

const COMMENT_A_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_04_01_000001_a_create_notes_comment ...................................  \n",
    "  \u{21c2} create table `notes` (`id` bigint unsigned not null auto_increment primary key, `body` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  \u{21c2} alter table `notes` comment = 'Notes kept by staff'  \n",
    "\n",
);

const COMMENT_A_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_04_01_000001_a_create_notes_comment .. \n",
    " \u{21c2} create table `notes` (`id` bigint unsigned not null auto_increment primary key, `body` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " \u{21c2} alter table `notes` comment = 'Notes kept by staff' \n",
    "\n",
);

const COMMENT_B_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_04_01_000002_b_table_comment ..........................................  \n",
    "  \u{21c2} alter table `notes` comment = 'Notes kept by staff'  \n",
    "\n",
);

const COMMENT_B_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_04_01_000002_b_table_comment .. \n",
    " \u{21c2} alter table `notes` comment = 'Notes kept by staff' \n",
    "\n",
);

const COMMENT_C_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_04_01_000003_c_table_comment_quote ....................................  \n",
    "  \u{21c2} alter table `notes` comment = 'It''s a note'  \n",
    "\n",
);

const COMMENT_C_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_04_01_000003_c_table_comment_quote .. \n",
    " \u{21c2} alter table `notes` comment = 'It''s a note' \n",
    "\n",
);

const COMMENT_D_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_04_01_000004_d_column_and_comment .....................................  \n",
    "  \u{21c2} alter table `notes` add `extra` varchar(255) null  \n",
    "  \u{21c2} alter table `notes` comment = 'x'  \n",
    "\n",
);

const COMMENT_D_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_04_01_000004_d_column_and_comment .. \n",
    " \u{21c2} alter table `notes` add `extra` varchar(255) null \n",
    " \u{21c2} alter table `notes` comment = 'x' \n",
    "\n",
);

const COMMENT_E_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_04_01_000005_e1_engine ................................................  \n",
    "\n",
    "  2026_04_01_000006_e2_charset ...............................................  \n",
    "\n",
    "  2026_04_01_000007_e3_collation .............................................  \n",
    "\n",
    "\n",
);

const COMMENT_E_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_04_01_000005_e1_engine .. \n",
    "\n",
    " 2026_04_01_000006_e2_charset .. \n",
    "\n",
    " 2026_04_01_000007_e3_collation .. \n",
    "\n",
    "\n",
);

const NOTES: &str = "create table `notes` (`id` bigint unsigned not null auto_increment, \
    `body` varchar(255) not null, primary key (`id`)) engine=InnoDB default charset=utf8mb4 \
    collate=utf8mb4_unicode_ci;\n";

const COMMENT_ON_NOTES: &str = "alter table `notes` comment = 'Notes kept by staff'";
const CREATE_NOTES: &str = "create table `notes` (`id` bigint unsigned not null auto_increment primary key, `body` varchar(255) not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'";

/// The five outputs in both forms: (label, output, migration, statements).
fn comment_outputs() -> Vec<(&'static str, &'static str, &'static str, Vec<&'static str>)> {
    vec![
        (
            "a long",
            COMMENT_A_LONG,
            "2026_04_01_000001_a_create_notes_comment",
            vec![CREATE_NOTES, COMMENT_ON_NOTES],
        ),
        (
            "a short",
            COMMENT_A_SHORT,
            "2026_04_01_000001_a_create_notes_comment",
            vec![CREATE_NOTES, COMMENT_ON_NOTES],
        ),
        (
            "b long",
            COMMENT_B_LONG,
            "2026_04_01_000002_b_table_comment",
            vec![COMMENT_ON_NOTES],
        ),
        (
            "b short",
            COMMENT_B_SHORT,
            "2026_04_01_000002_b_table_comment",
            vec![COMMENT_ON_NOTES],
        ),
        (
            "c long",
            COMMENT_C_LONG,
            "2026_04_01_000003_c_table_comment_quote",
            vec!["alter table `notes` comment = 'It''s a note'"],
        ),
        (
            "c short",
            COMMENT_C_SHORT,
            "2026_04_01_000003_c_table_comment_quote",
            vec!["alter table `notes` comment = 'It''s a note'"],
        ),
        (
            "d long",
            COMMENT_D_LONG,
            "2026_04_01_000004_d_column_and_comment",
            vec![
                "alter table `notes` add `extra` varchar(255) null",
                "alter table `notes` comment = 'x'",
            ],
        ),
        (
            "d short",
            COMMENT_D_SHORT,
            "2026_04_01_000004_d_column_and_comment",
            vec![
                "alter table `notes` add `extra` varchar(255) null",
                "alter table `notes` comment = 'x'",
            ],
        ),
    ]
}

#[test]
fn laravel_s_table_comment_statements_are_read_as_the_statements_of_their_migration() {
    for (label, output, migration, statements) in comment_outputs() {
        let pretend = pretend_of(output);
        assert!(pretend.unreadable.is_empty(), "{label}");
        assert_eq!(migration_names(&pretend), [migration], "{label}");
        assert_eq!(sql_of(&pretend.migrations[0]), statements, "{label}");
        assert_eq!(pretend.migrations[0].first_read, None, "{label}");
    }
}

#[test]
fn laravel_s_table_comment_gets_the_measured_answer_on_both_servers_in_both_forms() {
    for (label, output, _, statements) in comment_outputs() {
        // The table comes with the schema unless the migration creates it.
        let schema = if statements[0] == CREATE_NOTES {
            ""
        } else {
            NOTES
        };
        let expected: Vec<&str> = statements
            .iter()
            .map(|sql| {
                if sql.starts_with("create table") {
                    NOT_CHECKED
                } else if sql.contains("comment =") {
                    INPLACE
                } else {
                    INSTANT
                }
            })
            .collect();
        for server in ["mysql-8.0.46", "mysql-8.4.13"] {
            assert_eq!(
                answers(server, schema, output),
                expected,
                "{label} on {server}"
            );
        }
    }
}

#[test]
fn laravel_s_table_comment_leaves_the_table_known() {
    // Before step 11b the comment was an unreadable statement: the table was
    // lost, and the next ADD COLUMN carried the note about a FULLTEXT index
    // kilit could not see.
    let next = "alter table `notes` add `extra` varchar(255) null";
    for output in [
        long_form(&[(
            "2026_04_01_000002_b_table_comment",
            &[COMMENT_ON_NOTES, next],
        )]),
        short_form(&[(
            "2026_04_01_000002_b_table_comment",
            &[COMMENT_ON_NOTES, next],
        )]),
    ] {
        for server in ["mysql-8.0.46", "mysql-8.4.13"] {
            let notes = notes_by_statement(server, NOTES, &output);
            assert_eq!(notes.len(), 2);
            assert!(
                !notes[1].iter().any(|note| note.contains("FULLTEXT")),
                "{server}: {:?}",
                notes[1]
            );
        }
    }
}

#[test]
fn migrations_that_only_set_the_engine_the_charset_or_the_collation_print_no_statement() {
    for (label, output) in [("long", COMMENT_E_LONG), ("short", COMMENT_E_SHORT)] {
        let pretend = pretend_of(output);
        assert!(pretend.unreadable.is_empty(), "{label}");
        assert_eq!(
            pretend
                .migrations
                .iter()
                .map(|m| m.statements.len())
                .sum::<usize>(),
            0,
            "{label}"
        );
        for server in ["mysql-8.0.46", "mysql-8.4.13"] {
            assert!(
                answers(server, NOTES, output).is_empty(),
                "{label} on {server}"
            );
        }
    }
}

// Step 13a: `php artisan migrate --pretend` for six migrations, byte for byte as
// Laravel 12.71.0 prints it (the same bytes in Laravel 13.40.0 in a terminal)
// and as Laravel 13.40.0 prints it where it detects an automated environment
// (the short form). Each case has one Schema::create (table t_<case>) and one
// Schema::table('orders'), on a base with orders(id). Laravel 10.50.4 and
// 11.57.1 were read from their source, not run: their SQL is the same for
// every case but date-usecurrent, where typeDate ignores useCurrent() and
// writes no default.
//
//   date-current-date              date('d')->default(DB::raw('CURRENT_DATE'))
//   date-current-timestamp         date('d')->default(DB::raw('CURRENT_TIMESTAMP'))
//   datetime6-current-timestamp    dateTime('t', 6)->default(DB::raw('CURRENT_TIMESTAMP'))
//   timestamp3-usecurrent-onupdate timestamp('t', 3)->useCurrent()->useCurrentOnUpdate()
//   date-usecurrent                date('d')->useCurrent()
//   string-collation-comment       string('s')->collation('utf8mb4_bin')->comment('x')
//
// Real runs on MySQL 8.0.46 and 8.4.13 failed the first three, the CREATE and
// the ALTER alike (1064 for CURRENT_DATE, 1067 for the other two), and ran the
// other three.

const NOT_ACCEPTED: &str = "MySQL 8.0.46 and 8.4.13 do not accept";

const STEP13A_DATE_CURRENT_DATE_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_05_01_000001_create_date-current-date .................................  \n",
    "  \u{21c2} create table `t_date_current_date` (`id` bigint unsigned not null auto_increment primary key, `d` date not null default CURRENT_DATE) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  2026_05_01_000002_table_date-current-date ..................................  \n",
    "  \u{21c2} alter table `orders` add `d` date not null default CURRENT_DATE  \n",
    "\n",
);

const STEP13A_DATE_CURRENT_DATE_L13_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_05_01_000001_create_date-current-date .. \n",
    " \u{21c2} create table `t_date_current_date` (`id` bigint unsigned not null auto_increment primary key, `d` date not null default CURRENT_DATE) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " 2026_05_01_000002_table_date-current-date .. \n",
    " \u{21c2} alter table `orders` add `d` date not null default CURRENT_DATE \n",
    "\n",
);

const STEP13A_DATE_CURRENT_TIMESTAMP_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_05_01_000001_create_date-current-timestamp ............................  \n",
    "  \u{21c2} create table `t_date_current_timestamp` (`id` bigint unsigned not null auto_increment primary key, `d` date not null default CURRENT_TIMESTAMP) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  2026_05_01_000002_table_date-current-timestamp .............................  \n",
    "  \u{21c2} alter table `orders` add `d` date not null default CURRENT_TIMESTAMP  \n",
    "\n",
);

const STEP13A_DATE_CURRENT_TIMESTAMP_L13_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_05_01_000001_create_date-current-timestamp .. \n",
    " \u{21c2} create table `t_date_current_timestamp` (`id` bigint unsigned not null auto_increment primary key, `d` date not null default CURRENT_TIMESTAMP) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " 2026_05_01_000002_table_date-current-timestamp .. \n",
    " \u{21c2} alter table `orders` add `d` date not null default CURRENT_TIMESTAMP \n",
    "\n",
);

const STEP13A_DATETIME6_CURRENT_TIMESTAMP_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_05_01_000001_create_datetime6-current-timestamp .......................  \n",
    "  \u{21c2} create table `t_datetime6_current_timestamp` (`id` bigint unsigned not null auto_increment primary key, `t` datetime(6) not null default CURRENT_TIMESTAMP) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  2026_05_01_000002_table_datetime6-current-timestamp ........................  \n",
    "  \u{21c2} alter table `orders` add `t` datetime(6) not null default CURRENT_TIMESTAMP  \n",
    "\n",
);

const STEP13A_DATETIME6_CURRENT_TIMESTAMP_L13_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_05_01_000001_create_datetime6-current-timestamp .. \n",
    " \u{21c2} create table `t_datetime6_current_timestamp` (`id` bigint unsigned not null auto_increment primary key, `t` datetime(6) not null default CURRENT_TIMESTAMP) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " 2026_05_01_000002_table_datetime6-current-timestamp .. \n",
    " \u{21c2} alter table `orders` add `t` datetime(6) not null default CURRENT_TIMESTAMP \n",
    "\n",
);

const STEP13A_TIMESTAMP3_USE_CURRENT_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_05_01_000001_create_timestamp3-usecurrent-onupdate ....................  \n",
    "  \u{21c2} create table `t_timestamp3_usecurrent_onupdate` (`id` bigint unsigned not null auto_increment primary key, `t` timestamp(3) not null default CURRENT_TIMESTAMP(3) on update CURRENT_TIMESTAMP(3)) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  2026_05_01_000002_table_timestamp3-usecurrent-onupdate .....................  \n",
    "  \u{21c2} alter table `orders` add `t` timestamp(3) not null default CURRENT_TIMESTAMP(3) on update CURRENT_TIMESTAMP(3)  \n",
    "\n",
);

const STEP13A_TIMESTAMP3_USE_CURRENT_L13_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_05_01_000001_create_timestamp3-usecurrent-onupdate .. \n",
    " \u{21c2} create table `t_timestamp3_usecurrent_onupdate` (`id` bigint unsigned not null auto_increment primary key, `t` timestamp(3) not null default CURRENT_TIMESTAMP(3) on update CURRENT_TIMESTAMP(3)) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " 2026_05_01_000002_table_timestamp3-usecurrent-onupdate .. \n",
    " \u{21c2} alter table `orders` add `t` timestamp(3) not null default CURRENT_TIMESTAMP(3) on update CURRENT_TIMESTAMP(3) \n",
    "\n",
);

const STEP13A_DATE_USE_CURRENT_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_05_01_000001_create_date-usecurrent ...................................  \n",
    "  \u{21c2} create table `t_date_usecurrent` (`id` bigint unsigned not null auto_increment primary key, `d` date not null default (CURDATE())) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  2026_05_01_000002_table_date-usecurrent ....................................  \n",
    "  \u{21c2} alter table `orders` add `d` date not null default (CURDATE())  \n",
    "\n",
);

const STEP13A_DATE_USE_CURRENT_L13_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_05_01_000001_create_date-usecurrent .. \n",
    " \u{21c2} create table `t_date_usecurrent` (`id` bigint unsigned not null auto_increment primary key, `d` date not null default (CURDATE())) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " 2026_05_01_000002_table_date-usecurrent .. \n",
    " \u{21c2} alter table `orders` add `d` date not null default (CURDATE()) \n",
    "\n",
);

const STEP13A_STRING_COLLATION_COMMENT_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_05_01_000001_create_string-collation-comment ..........................  \n",
    "  \u{21c2} create table `t_string_collation_comment` (`id` bigint unsigned not null auto_increment primary key, `s` varchar(255) collate 'utf8mb4_bin' not null comment 'x') default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  2026_05_01_000002_table_string-collation-comment ...........................  \n",
    "  \u{21c2} alter table `orders` add `s` varchar(255) collate 'utf8mb4_bin' not null comment 'x'  \n",
    "\n",
);

const STEP13A_STRING_COLLATION_COMMENT_L13_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2026_05_01_000001_create_string-collation-comment .. \n",
    " \u{21c2} create table `t_string_collation_comment` (`id` bigint unsigned not null auto_increment primary key, `s` varchar(255) collate 'utf8mb4_bin' not null comment 'x') default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " 2026_05_01_000002_table_string-collation-comment .. \n",
    " \u{21c2} alter table `orders` add `s` varchar(255) collate 'utf8mb4_bin' not null comment 'x' \n",
    "\n",
);

const STEP13A_DATE_USE_CURRENT_L10: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2026_05_01_000001_create_date-usecurrent ...................................  \n",
    "  \u{21c2} create table `t_date_usecurrent` (`id` bigint unsigned not null auto_increment primary key, `d` date not null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  2026_05_01_000002_table_date-usecurrent ....................................  \n",
    "  \u{21c2} alter table `orders` add `d` date not null  \n",
    "\n",
);

/// The reason of each statement kilit cannot answer, in order; None where it
/// has an answer.
fn unknown_reasons(server: &str, schema: &str, output: &str) -> Vec<Option<String>> {
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
            Prediction::Unknown { reason, .. } => Some(reason),
            _ => None,
        })
        .collect()
}

#[test]
fn laravel_s_default_raw_that_mysql_rejects_is_unknown_for_the_create_and_for_the_table() {
    // $table->date('d')->default(DB::raw('CURRENT_DATE')) and
    // ->default(DB::raw('CURRENT_TIMESTAMP')) on a date,
    // $table->dateTime('t', 6)->default(DB::raw('CURRENT_TIMESTAMP')):
    // MySQL 8.0.46 and 8.4.13 refuse the Schema::create and the Schema::table
    // alike (hand tests g1, g23, g24 and a, e).
    for (case, long, short) in [
        (
            "date-current-date",
            STEP13A_DATE_CURRENT_DATE_L12,
            STEP13A_DATE_CURRENT_DATE_L13_SHORT,
        ),
        (
            "date-current-timestamp",
            STEP13A_DATE_CURRENT_TIMESTAMP_L12,
            STEP13A_DATE_CURRENT_TIMESTAMP_L13_SHORT,
        ),
        (
            "datetime6-current-timestamp",
            STEP13A_DATETIME6_CURRENT_TIMESTAMP_L12,
            STEP13A_DATETIME6_CURRENT_TIMESTAMP_L13_SHORT,
        ),
    ] {
        for (form, output) in [("long", long), ("short", short)] {
            assert_eq!(pretend_of(output).migrations.len(), 2, "{case} {form}");
            for server in ["mysql-8.0.46", "mysql-8.4.13"] {
                assert_eq!(
                    answers(server, ORDERS, output),
                    ["unknown", "unknown"],
                    "{case} {form} on {server}"
                );
                for (statement, reason) in unknown_reasons(server, ORDERS, output)
                    .into_iter()
                    .enumerate()
                {
                    let reason = reason.unwrap();
                    assert!(
                        reason.starts_with(NOT_ACCEPTED),
                        "{case} {form} on {server}, statement {statement}: {reason}"
                    );
                }
            }
        }
    }
}

#[test]
fn laravel_s_use_current_and_use_current_on_update_keep_their_answers() {
    // $table->timestamp('t', 3)->useCurrent()->useCurrentOnUpdate(): the
    // precision matches, MySQL runs both statements (adding the column is
    // INSTANT). $table->date('d')->useCurrent() on Laravel 12 and 13 writes
    // default (CURDATE()), an expression default that copies the table.
    for (form, output) in [
        ("long", STEP13A_TIMESTAMP3_USE_CURRENT_L12),
        ("short", STEP13A_TIMESTAMP3_USE_CURRENT_L13_SHORT),
    ] {
        for server in ["mysql-8.0.46", "mysql-8.4.13"] {
            assert_eq!(
                answers(server, ORDERS, output),
                [NOT_CHECKED, INSTANT],
                "timestamp3 {form} on {server}"
            );
        }
    }
    for (form, output) in [
        ("long", STEP13A_DATE_USE_CURRENT_L12),
        ("short", STEP13A_DATE_USE_CURRENT_L13_SHORT),
    ] {
        for server in ["mysql-8.0.46", "mysql-8.4.13"] {
            assert_eq!(
                answers(server, ORDERS, output),
                [NOT_CHECKED, COPY],
                "date {form} on {server}"
            );
        }
    }
    // Laravel 10 and 11: no default at all, a NOT NULL date column. B1c,
    // The lab has not measured that shape, so it is unknown.
    for server in ["mysql-8.0.46", "mysql-8.4.13"] {
        assert_eq!(
            answers(server, ORDERS, STEP13A_DATE_USE_CURRENT_L10),
            [NOT_CHECKED, "unknown"],
            "date on Laravel 10 on {server}"
        );
    }
}

#[test]
fn laravel_s_collation_and_comment_on_a_string_keep_their_answers() {
    // $table->string('s')->collation('utf8mb4_bin')->comment('x')
    for (form, output) in [
        ("long", STEP13A_STRING_COLLATION_COMMENT_L12),
        ("short", STEP13A_STRING_COLLATION_COMMENT_L13_SHORT),
    ] {
        for server in ["mysql-8.0.46", "mysql-8.4.13"] {
            assert_eq!(
                answers(server, ORDERS, output),
                [NOT_CHECKED, INSTANT],
                "{form} on {server}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Laravel's stop at its confirmation for an application in production.
//
// With APP_ENV set to production or not set, and without --force, `migrate`
// asks "Are you sure you want to run this command?" before it does anything.
// Without a terminal to answer, or with the answer no, it prints
// "APPLICATION IN PRODUCTION." and "Command cancelled." and nothing ran.
//
// The captures are real runs of Laravel 12.71.0 and 13.40.0 on PHP 8.3. The
// output of Laravel 10.25 and later and of Laravel 11 is derived from the
// source of v10.50.4 and v11.57.1, which has the same code, so it is the
// bytes of Laravel 12 (not a real run). Laravel 10.0 to 10.24 print other
// bytes: see the block at the end of this file. The Windows question is a SIMULATED run: `windows_os()` forced
// true so that Symfony asks instead of Laravel Prompts; it is not a real
// Windows run.
// ---------------------------------------------------------------------------

use kilit_core::LaravelError;

/// `php artisan migrate --pretend > out` on Laravel 12.71.0, APP_ENV not set,
/// no terminal: the box, then the cancel line. Laravel 10.25 and later and
/// Laravel 11 print the same (derived from the source, not run); 10.0 to 10.24
/// do not.
const CONFIRM_STOP_L12_PIPED: &str = concat!(
    "\n",
    "                                                                                \n",
    "                           APPLICATION IN PRODUCTION.                           \n",
    "                                                                                \n",
    "\n",
    "   WARN  Command cancelled.  \n",
    "\n",
);

/// Laravel 13.40.0 where it detects an automated environment.
const CONFIRM_STOP_L13_SHORT: &str = concat!(
    "\n",
    " APPLICATION IN PRODUCTION. \n",
    " \n",
    "\n",
    " WARN Command cancelled. \n",
    "\n",
);

/// Laravel 13.40.0, long form.
const CONFIRM_STOP_L13_LONG: &str = concat!(
    "\n",
    "                                                                                \n",
    "                           APPLICATION IN PRODUCTION.                           \n",
    "                                                                                \n",
    "\n",
    "   WARN  Command cancelled.  \n",
    "\n",
);

/// `--ansi` on Laravel 12.71.0: the box and the label in color.
const CONFIRM_STOP_L12_ANSI: &str = concat!(
    "\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \n",
    "  \u{1b}[30;43m                         APPLICATION IN PRODUCTION.                         \u{1b}[39;49m  \n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \n",
    "\n",
    "  \u{1b}[30;43m WARN \u{1b}[39;49m Command cancelled.  \n",
    "\n",
);

/// `COLUMNS=10` on Laravel 12.71.0: the warning is cut to `APPLIC`.
const CONFIRM_STOP_L12_COLUMNS_10: &str = concat!(
    "\n",
    "          \n",
    "  APPLIC  \n",
    "          \n",
    "\n",
    "   WARN  Command cancelled.  \n",
    "\n",
);

/// Laravel 12.71.0 in a terminal, answered `n`: every byte the terminal got.
const CONFIRM_STOP_L12_TERMINAL_NO: &str = concat!(
    "\r\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \r\n",
    "  \u{1b}[30;43m                         APPLICATION IN PRODUCTION.                         \u{1b}[39;49m  \r\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \r\n",
    "\u{1b}[?25l\r\n",
    "\u{1b}[90m ┌\u{1b}[39m \u{1b}[36mAre you sure you want to run this command?\u{1b}[39m \u{1b}[90m──────────────────┐\u{1b}[39m\r\n",
    "\u{1b}[90m │\u{1b}[39m \u{1b}[2m○ Yes /\u{1b}[22m \u{1b}[32m●\u{1b}[39m No                                                 \u{1b}[90m│\u{1b}[39m\r\n",
    "\u{1b}[90m └──────────────────────────────────────────────────────────────┘\u{1b}[39m\r\n",
    "\r\n",
    "\u{1b}[1G\u{1b}[5A\u{1b}[J\r\n",
    "\u{1b}[90m ┌\u{1b}[39m \u{1b}[2mAre you sure you want to run this command?\u{1b}[22m \u{1b}[90m──────────────────┐\u{1b}[39m\r\n",
    "\u{1b}[90m │\u{1b}[39m No                                                           \u{1b}[90m│\u{1b}[39m\r\n",
    "\u{1b}[90m └──────────────────────────────────────────────────────────────┘\u{1b}[39m\r\n",
    "\r\n",
    "\u{1b}[?25h  \u{1b}[30;43m WARN \u{1b}[39;49m Command cancelled.  \r\n",
    "\r\n",
);

/// Laravel 13.40.0 in a terminal where it detects an automated environment,
/// answered `n`.
const CONFIRM_STOP_L13_SHORT_TERMINAL_NO: &str = concat!(
    "\r\n",
    " APPLICATION IN PRODUCTION. \r\n",
    " \r\n",
    "\u{1b}[?25l\r\n",
    " Are you sure you want to run this command? \r\n",
    " ○ Yes / No \r\n",
    "\u{1b}[1G\u{1b}[5A\u{1b}[J\r\n",
    " Are you sure you want to run this command? \r\n",
    " No \r\n",
    "\u{1b}[?25h\r\n",
    " WARN Command cancelled. \r\n",
    "\r\n",
);

/// Laravel 12.71.0 with the terminal as input and stdout in a pipe, answered
/// `n`: the question is drawn in the pipe too.
const CONFIRM_STOP_L12_PIPE_AFTER_TERMINAL_NO: &str = concat!(
    "\n",
    "                                                                                \n",
    "                           APPLICATION IN PRODUCTION.                           \n",
    "                                                                                \n",
    "\u{1b}[?25l\n",
    "\u{1b}[90m ┌\u{1b}[39m \u{1b}[36mAre you sure you want to run this command?\u{1b}[39m \u{1b}[90m──────────────────┐\u{1b}[39m\n",
    "\u{1b}[90m │\u{1b}[39m \u{1b}[2m○ Yes /\u{1b}[22m \u{1b}[32m●\u{1b}[39m No                                                 \u{1b}[90m│\u{1b}[39m\n",
    "\u{1b}[90m └──────────────────────────────────────────────────────────────┘\u{1b}[39m\n",
    "\n",
    "\u{1b}[1G\u{1b}[5A\u{1b}[J\n",
    "\u{1b}[90m ┌\u{1b}[39m \u{1b}[2mAre you sure you want to run this command?\u{1b}[22m \u{1b}[90m──────────────────┐\u{1b}[39m\n",
    "\u{1b}[90m │\u{1b}[39m No                                                           \u{1b}[90m│\u{1b}[39m\n",
    "\u{1b}[90m └──────────────────────────────────────────────────────────────┘\u{1b}[39m\n",
    "\n",
    "\u{1b}[?25h   WARN  Command cancelled.  \n",
    "\n",
);

/// SIMULATED Windows form on Laravel 12.71.0 (`windows_os()` forced true, so
/// Symfony asks `(yes/no) [no]` instead of Laravel Prompts); not a real
/// Windows run.
const CONFIRM_STOP_L12_WINDOWS_QUESTION: &str = concat!(
    "\n",
    "                                                                                \n",
    "                           APPLICATION IN PRODUCTION.                           \n",
    "                                                                                \n",
    "\n",
    "  Are you sure you want to run this command? (yes/no) [no]\n",
    "❯ \n",
    "   WARN  Command cancelled.  \n",
    "\n",
);

/// The same simulated Windows form in a terminal, answered `no`.
const CONFIRM_STOP_L12_WINDOWS_QUESTION_NO: &str = concat!(
    "\r\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \r\n",
    "  \u{1b}[30;43m                         APPLICATION IN PRODUCTION.                         \u{1b}[39;49m  \r\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \r\n",
    "\r\n",
    "\u{1b}[32m  \u{1b}[39m\u{1b}[39;1mAre you sure you want to run this command?\u{1b}[39;22m (yes/no) [\u{1b}[33mno\u{1b}[39m]\r\n",
    "\u{1b}[1m❯ \u{1b}[22mno\r\n",
    "\r\n",
    "  \u{1b}[30;43m WARN \u{1b}[39;49m Command cancelled.  \r\n",
    "\r\n",
);

/// Laravel 13.40.0, APP_ENV not set, every migration already applied: the
/// confirmation comes before Laravel looks at the migrations.
const NOTHING_TO_MIGRATE_IN_PRODUCTION_L13: &str = concat!(
    "\n",
    "                                                                                \n",
    "                           APPLICATION IN PRODUCTION.                           \n",
    "                                                                                \n",
    "\n",
    "   WARN  Command cancelled.  \n",
    "\n",
);

/// Laravel 12.71.0 in a terminal, answered `y`: the migration ran under
/// --pretend.
const CONFIRM_YES_L12_TERMINAL: &str = concat!(
    "\r\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \r\n",
    "  \u{1b}[30;43m                         APPLICATION IN PRODUCTION.                         \u{1b}[39;49m  \r\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \r\n",
    "\u{1b}[?25l\r\n",
    "\u{1b}[90m ┌\u{1b}[39m \u{1b}[36mAre you sure you want to run this command?\u{1b}[39m \u{1b}[90m──────────────────┐\u{1b}[39m\r\n",
    "\u{1b}[90m │\u{1b}[39m \u{1b}[2m○ Yes /\u{1b}[22m \u{1b}[32m●\u{1b}[39m No                                                 \u{1b}[90m│\u{1b}[39m\r\n",
    "\u{1b}[90m └──────────────────────────────────────────────────────────────┘\u{1b}[39m\r\n",
    "\r\n",
    "\u{1b}[1G\u{1b}[5A\u{1b}[J\r\n",
    "\u{1b}[90m ┌\u{1b}[39m \u{1b}[36mAre you sure you want to run this command?\u{1b}[39m \u{1b}[90m──────────────────┐\u{1b}[39m\r\n",
    "\u{1b}[90m │\u{1b}[39m \u{1b}[32m●\u{1b}[39m Yes \u{1b}[2m/ ○ No\u{1b}[22m                                                 \u{1b}[90m│\u{1b}[39m\r\n",
    "\u{1b}[90m └──────────────────────────────────────────────────────────────┘\u{1b}[39m\r\n",
    "\r\n",
    "\u{1b}[1G\u{1b}[5A\u{1b}[J\r\n",
    "\u{1b}[90m ┌\u{1b}[39m \u{1b}[2mAre you sure you want to run this command?\u{1b}[22m \u{1b}[90m──────────────────┐\u{1b}[39m\r\n",
    "\u{1b}[90m │\u{1b}[39m Yes                                                          \u{1b}[90m│\u{1b}[39m\r\n",
    "\u{1b}[90m └──────────────────────────────────────────────────────────────┘\u{1b}[39m\r\n",
    "\r\n",
    "\u{1b}[?25h  \u{1b}[37;44m INFO \u{1b}[39;49m Running migrations.  \r\n",
    "\r\n",
    "  2025_03_01_000000_widen_orders_amount \u{1b}[90m......................................\u{1b}[39m  \r\n",
    "  \u{1b}[90m⇂ alter table `orders` modify `amount` bigint unsigned not null\u{1b}[39m  \r\n",
    "  \u{1b}[90m⇂ alter table `orders` add index `orders_user_id_idx`(`user_id`)\u{1b}[39m  \r\n",
    "\r\n",
);

/// The SIMULATED Windows form answered `yes` (not a real Windows run).
const CONFIRM_YES_L12_WINDOWS_QUESTION: &str = concat!(
    "\r\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \r\n",
    "  \u{1b}[30;43m                         APPLICATION IN PRODUCTION.                         \u{1b}[39;49m  \r\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \r\n",
    "\r\n",
    "\u{1b}[32m  \u{1b}[39m\u{1b}[39;1mAre you sure you want to run this command?\u{1b}[39;22m (yes/no) [\u{1b}[33mno\u{1b}[39m]\r\n",
    "\u{1b}[1m❯ \u{1b}[22myes\r\n",
    "\r\n",
    "  \u{1b}[37;44m INFO \u{1b}[39;49m Running migrations.  \r\n",
    "\r\n",
    "  2025_03_01_000000_widen_orders_amount \u{1b}[90m......................................\u{1b}[39m  \r\n",
    "  \u{1b}[90m⇂ alter table `orders` modify `amount` bigint unsigned not null\u{1b}[39m  \r\n",
    "  \u{1b}[90m⇂ alter table `orders` add index `orders_user_id_idx`(`user_id`)\u{1b}[39m  \r\n",
    "\r\n",
);

/// Laravel 12.71.0, APP_ENV=production with `--force`: no question.
const PRODUCTION_FORCE_L12: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_03_01_000000_widen_orders_amount ......................................  \n",
    "  ⇂ alter table `orders` modify `amount` bigint unsigned not null  \n",
    "  ⇂ alter table `orders` add index `orders_user_id_idx`(`user_id`)  \n",
    "\n",
);

/// Laravel 13.40.0, APP_ENV=testing: no question.
const TESTING_L13: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_03_01_000000_widen_orders_amount ......................................  \n",
    "  ⇂ alter table `orders` modify `amount` bigint unsigned not null  \n",
    "  ⇂ alter table `orders` add index `orders_user_id_idx`(`user_id`)  \n",
    "\n",
);

const CONFIRM_STOP_MESSAGE: &str = "Laravel stopped at its confirmation for an application in \
     production (APP_ENV is production or not set) and cancelled the command; no statement was seen";

/// Each stop with the line of its warning, counting from 1; the cancel line
/// when the output has no warning.
const CONFIRM_STOPS: [(&str, &str, usize); 12] = [
    (
        "piped, Laravel 12 (and 10.25 on, 11)",
        CONFIRM_STOP_L12_PIPED,
        3,
    ),
    ("piped, Laravel 13", CONFIRM_STOP_L13_LONG, 3),
    ("piped, shorter form", CONFIRM_STOP_L13_SHORT, 2),
    ("--ansi", CONFIRM_STOP_L12_ANSI, 3),
    ("COLUMNS=10", CONFIRM_STOP_L12_COLUMNS_10, 3),
    ("terminal, answer n", CONFIRM_STOP_L12_TERMINAL_NO, 3),
    (
        "terminal, answer n, shorter form",
        CONFIRM_STOP_L13_SHORT_TERMINAL_NO,
        2,
    ),
    (
        "terminal in, pipe out, answer n",
        CONFIRM_STOP_L12_PIPE_AFTER_TERMINAL_NO,
        3,
    ),
    (
        "simulated Windows question, no terminal",
        CONFIRM_STOP_L12_WINDOWS_QUESTION,
        3,
    ),
    (
        "simulated Windows question, answer no",
        CONFIRM_STOP_L12_WINDOWS_QUESTION_NO,
        3,
    ),
    (
        "nothing to migrate",
        NOTHING_TO_MIGRATE_IN_PRODUCTION_L13,
        3,
    ),
    ("cancel line alone", "\n   WARN  Command cancelled.  \n", 2),
];

#[test]
fn laravel_stopping_at_the_production_confirmation_is_an_error_on_the_warning_s_line() {
    for (form, output, line) in CONFIRM_STOPS {
        let pretend =
            read_pretend(output).unwrap_or_else(|| panic!("{form}: not Laravel's output"));
        assert!(pretend.migrations.is_empty(), "{form}");
        assert!(pretend.unreadable.is_empty(), "{form}");
        assert_eq!(
            pretend.error,
            Some(LaravelError {
                line,
                migration: None,
                message: CONFIRM_STOP_MESSAGE.into(),
            }),
            "{form}"
        );
    }
}

#[test]
fn the_question_laravel_draws_in_a_terminal_is_not_read_as_a_statement() {
    // The box, the question drawn twice with cursor moves, and the answer.
    for (form, output) in [
        ("Laravel Prompts", CONFIRM_STOP_L12_TERMINAL_NO),
        (
            "Laravel Prompts, shorter form",
            CONFIRM_STOP_L13_SHORT_TERMINAL_NO,
        ),
        ("Symfony question", CONFIRM_STOP_L12_WINDOWS_QUESTION),
    ] {
        let pretend = read_pretend(output).unwrap();
        assert!(pretend.unreadable.is_empty(), "{form}");
        assert!(pretend.migrations.is_empty(), "{form}");
    }
}

#[test]
fn answering_yes_at_the_production_confirmation_is_not_a_stop() {
    // The migration ran under --pretend: its statements are read and Laravel
    // did not stop. The warning and the question lines are lines kilit
    // cannot read, as before.
    for (form, output) in [
        ("Laravel Prompts", CONFIRM_YES_L12_TERMINAL),
        (
            "simulated Windows question",
            CONFIRM_YES_L12_WINDOWS_QUESTION,
        ),
    ] {
        let pretend = read_pretend(output).unwrap();
        assert_eq!(pretend.error, None, "{form}");
        assert_eq!(
            migration_names(&pretend),
            ["2025_03_01_000000_widen_orders_amount"],
            "{form}"
        );
        assert_eq!(
            sql_of(&pretend.migrations[0]),
            [
                "alter table `orders` modify `amount` bigint unsigned not null",
                "alter table `orders` add index `orders_user_id_idx`(`user_id`)",
            ],
            "{form}"
        );
        assert!(!pretend.unreadable.is_empty(), "{form}");
        assert!(
            pretend
                .unreadable
                .iter()
                .any(|line| line.text.contains("APPLICATION IN PRODUCTION")),
            "{form}: {:?}",
            pretend.unreadable
        );
        assert!(
            pretend
                .unreadable
                .iter()
                .all(|line| !line.text.contains("cancelled")),
            "{form}: {:?}",
            pretend.unreadable
        );
    }
}

#[test]
fn migrations_run_with_force_or_outside_production_are_read_without_an_error() {
    // APP_ENV=production with --force, and APP_ENV=testing: no question.
    for (form, output) in [
        ("production --force", PRODUCTION_FORCE_L12),
        ("testing", TESTING_L13),
    ] {
        let pretend = read_pretend(output).unwrap();
        assert_eq!(pretend.error, None, "{form}");
        assert!(pretend.unreadable.is_empty(), "{form}");
        assert_eq!(
            migration_names(&pretend),
            ["2025_03_01_000000_widen_orders_amount"],
            "{form}"
        );
        assert_eq!(sql_of(&pretend.migrations[0]).len(), 2, "{form}");
    }
}

// ---------------------------------------------------------------------------
// Laravel 10.0 to 10.24: the stop at the production confirmation before
// Laravel 10.25.
//
// DERIVED, not real runs: the framework's own console classes of v10.0.0,
// v10.16.1, v10.17.0 and v10.24.0 run in a harness (symfony/console 6.4,
// termwind, laravel/prompts), with the Laravel 12.71.0 runs as the check of
// the harness.
//
// Laravel 10.0 to 10.16 print `Command canceled.` (one l) and, when stdin is
// not a terminal and there is no -n, Symfony's question between the warning
// and the cancel line. With -n the two question lines are absent. v10.17.0 is
// the first tag with `Command cancelled.` and Laravel Prompts; from v10.17 to
// v10.24 with -n there is no blank line between the warning and the cancel
// line. From v10.25.0 on the bytes are those of Laravel 12.
// ---------------------------------------------------------------------------

/// v10.0.0 and v10.16.1 (the same bytes), `php artisan migrate --pretend > out`,
/// stdin not a terminal, no -n: Symfony's question and its empty answer.
const CONFIRM_STOP_L10_0_PIPED: &str = concat!(
    "\n",
    "                                                                                \n",
    "                           APPLICATION IN PRODUCTION.                           \n",
    "                                                                                \n",
    "\n",
    "  Do you really wish to run this command? (yes/no) [no]\n",
    "❯ \n",
    "   WARN  Command canceled.  \n",
    "\n",
);

/// v10.0.0 and v10.16.1 with `-n`: no question, and `canceled` with one l.
const CONFIRM_STOP_L10_0_NO_INTERACTION: &str = concat!(
    "\n",
    "                                                                                \n",
    "                           APPLICATION IN PRODUCTION.                           \n",
    "                                                                                \n",
    "\n",
    "   WARN  Command canceled.  \n",
    "\n",
);

/// v10.0.0 and v10.16.1 with `--ansi`.
const CONFIRM_STOP_L10_0_ANSI: &str = concat!(
    "\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \n",
    "  \u{1b}[30;43m                         APPLICATION IN PRODUCTION.                         \u{1b}[39;49m  \n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \n",
    "\n",
    "\u{1b}[32m  \u{1b}[39m\u{1b}[39;1mDo you really wish to run this command?\u{1b}[39;22m (yes/no) [\u{1b}[33mno\u{1b}[39m]\n",
    "\u{1b}[1m❯ \u{1b}[22m\n",
    "  \u{1b}[30;43m WARN \u{1b}[39;49m Command canceled.  \n",
    "\n",
);

/// v10.0.0 and v10.16.1 in a terminal, answered `n` (the harness' own last
/// line, `[sim exit 1]`, left out).
const CONFIRM_STOP_L10_0_TERMINAL_NO: &str = concat!(
    "\r\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \r\n",
    "  \u{1b}[30;43m                         APPLICATION IN PRODUCTION.                         \u{1b}[39;49m  \r\n",
    "  \u{1b}[30;43m                                                                            \u{1b}[39;49m  \r\n",
    "\r\n",
    "\u{1b}[32m  \u{1b}[39m\u{1b}[39;1mDo you really wish to run this command?\u{1b}[39;22m (yes/no) [\u{1b}[33mno\u{1b}[39m]\r\n",
    "\u{1b}[1m❯ \u{1b}[22mn\r\n",
    "\r\n",
    "\r\n",
    "  \u{1b}[30;43m WARN \u{1b}[39;49m Command canceled.  \r\n",
    "\r\n",
);

/// v10.17.0 to v10.24.0 with `-n`: `cancelled`, and no blank line between the
/// warning and the cancel line.
const CONFIRM_STOP_L10_17_NO_INTERACTION: &str = concat!(
    "\n",
    "                                                                                \n",
    "                           APPLICATION IN PRODUCTION.                           \n",
    "                                                                                \n",
    "   WARN  Command cancelled.  \n",
    "\n",
);

const CONFIRM_STOPS_BEFORE_LARAVEL_10_25: [(&str, &str); 5] = [
    (
        "10.0 to 10.16, piped, Symfony's question",
        CONFIRM_STOP_L10_0_PIPED,
    ),
    (
        "10.0 to 10.16, -n, canceled with one l",
        CONFIRM_STOP_L10_0_NO_INTERACTION,
    ),
    ("10.0 to 10.16, --ansi", CONFIRM_STOP_L10_0_ANSI),
    (
        "10.0 to 10.16, terminal, answer n",
        CONFIRM_STOP_L10_0_TERMINAL_NO,
    ),
    (
        "10.17 to 10.24, -n, no blank line",
        CONFIRM_STOP_L10_17_NO_INTERACTION,
    ),
];

#[test]
fn laravel_10_stopping_at_the_production_confirmation_before_10_25_is_an_error_on_the_warning_s_line()
 {
    // Derived (see the block comment above), not real runs. The warning is
    // on line 3 in every form.
    for (form, output) in CONFIRM_STOPS_BEFORE_LARAVEL_10_25 {
        let pretend =
            read_pretend(output).unwrap_or_else(|| panic!("{form}: not Laravel's output"));
        assert!(pretend.migrations.is_empty(), "{form}");
        assert!(
            pretend.unreadable.is_empty(),
            "{form}: {:?}",
            pretend.unreadable
        );
        assert_eq!(
            pretend.error,
            Some(LaravelError {
                line: 3,
                migration: None,
                message: CONFIRM_STOP_MESSAGE.into(),
            }),
            "{form}"
        );
    }
}

// ---------------------------------------------------------------------------
// Step B1b: what Laravel prints for `->default()` on text, json and blob
// columns, and the text a migration prints itself (echo, print, dump(), the
// console's writeln). The outputs are real runs of `php artisan migrate
// --pretend` on Laravel 12.71.0 and 13.40.0 (the long form, and the short
// form of 13.40.0), bytes as printed; the printed-text forms are the same in
// Laravel 10 to 13, whose source prints it the same way. The one Laravel 10
// output is derived from the framework's source at 10.50.4 and was not run:
// Laravel 10 joins the columns of one Schema::table into one ALTER TABLE.

/// Laravel 12.71.0 (the same bytes in 13.40.0): unit3d's real migration, which prints nine lines of its own.
const B1B_UNIT3D_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2024_06_06_042258_create_private_message_conversations .....................  \n",
    "\n",
    "\tThis migration is long and will take approximately 30-60 seconds per 100 thousand stored private messages.\n",
    "\tCreating new tables and columns...\n",
    "\tParsing previous private message structure...\n",
    "\tInserting all conversations into new conversations table...\n",
    "\tAdding conversation_id to all private messages...\n",
    "\tInserting all conversation participants...\n",
    "\tUpdating all conversation timestamps...\n",
    "\tUpdating all participant timestamps...\n",
    "\tDropping unused columns from private messages and adding constraints...\n",
    "  ⇂ create table `conversations` (`id` int unsigned not null auto_increment primary key, `subject` varchar(255) not null, `created_at` timestamp null, `updated_at` timestamp null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  ⇂ alter table `conversations` add index `conversations_updated_at_index`(`updated_at`)  \n",
    "  ⇂ alter table `conversations` add index `conversations_created_at_index`(`created_at`)  \n",
    "  ⇂ create table `participants` (`id` int unsigned not null auto_increment primary key, `conversation_id` int unsigned not null, `user_id` int unsigned not null, `read` tinyint(1) not null default '0', `deleted_at` timestamp null, `created_at` timestamp null, `updated_at` timestamp null) default character set utf8mb4 collate 'utf8mb4_unicode_ci'  \n",
    "  ⇂ alter table `participants` add unique `participants_user_id_conversation_id_unique`(`user_id`, `conversation_id`)  \n",
    "  ⇂ alter table `participants` add index `participants_user_id_read_deleted_at_index`(`user_id`, `read`, `deleted_at`)  \n",
    "  ⇂ alter table `participants` add constraint `participants_conversation_id_foreign` foreign key (`conversation_id`) references `conversations` (`id`) on delete cascade on update cascade  \n",
    "  ⇂ alter table `participants` add constraint `participants_user_id_foreign` foreign key (`user_id`) references `users` (`id`) on delete cascade on update cascade  \n",
    "  ⇂ alter table `participants` add index `participants_conversation_id_index`(`conversation_id`)  \n",
    "  ⇂ alter table `private_messages` add `conversation_id` int unsigned null after `id`  \n",
    "  ⇂ select `id`, `sender_id`, `receiver_id`, `subject`, `read`, `related_to`, `created_at`, `updated_at` from `private_messages` where `id` is not null order by `id` desc limit 1000  \n",
    "  ⇂ update `conversations` inner join (select `conversation_id`, MIN(created_at) AS min_created_at from `private_messages` group by `conversation_id`) as `oldest_messages` on `oldest_messages`.`conversation_id` = `conversations`.`id` set `conversations`.`created_at` = oldest_messages.min_created_at  \n",
    "  ⇂ update `participants` inner join `conversations` on `conversations`.`id` = `participants`.`conversation_id` set `participants`.`created_at` = conversations.created_at  \n",
    "  ⇂ alter table `private_messages` modify `conversation_id` int unsigned not null  \n",
    "  ⇂ alter table `private_messages` drop foreign key `private_messages_receiver_id_foreign`  \n",
    "  ⇂ alter table `private_messages` drop `receiver_id`, drop `subject`, drop `related_to`, drop `read`  \n",
    "  ⇂ alter table `private_messages` add constraint `private_messages_conversation_id_foreign` foreign key (`conversation_id`) references `conversations` (`id`) on delete cascade on update cascade  \n",
    "\n",
);

/// Laravel 13.40.0, the short form: unit3d's real migration.
const B1B_UNIT3D_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2024_06_06_042258_create_private_message_conversations .. \n",
    "\n",
    "\tThis migration is long and will take approximately 30-60 seconds per 100 thousand stored private messages.\n",
    "\tCreating new tables and columns...\n",
    "\tParsing previous private message structure...\n",
    "\tInserting all conversations into new conversations table...\n",
    "\tAdding conversation_id to all private messages...\n",
    "\tInserting all conversation participants...\n",
    "\tUpdating all conversation timestamps...\n",
    "\tUpdating all participant timestamps...\n",
    "\tDropping unused columns from private messages and adding constraints...\n",
    " ⇂ create table `conversations` (`id` int unsigned not null auto_increment primary key, `subject` varchar(255) not null, `created_at` timestamp null, `updated_at` timestamp null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " ⇂ alter table `conversations` add index `conversations_updated_at_index`(`updated_at`) \n",
    " ⇂ alter table `conversations` add index `conversations_created_at_index`(`created_at`) \n",
    " ⇂ create table `participants` (`id` int unsigned not null auto_increment primary key, `conversation_id` int unsigned not null, `user_id` int unsigned not null, `read` tinyint(1) not null default '0', `deleted_at` timestamp null, `created_at` timestamp null, `updated_at` timestamp null) default character set utf8mb4 collate 'utf8mb4_unicode_ci' \n",
    " ⇂ alter table `participants` add unique `participants_user_id_conversation_id_unique`(`user_id`, `conversation_id`) \n",
    " ⇂ alter table `participants` add index `participants_user_id_read_deleted_at_index`(`user_id`, `read`, `deleted_at`) \n",
    " ⇂ alter table `participants` add constraint `participants_conversation_id_foreign` foreign key (`conversation_id`) references `conversations` (`id`) on delete cascade on update cascade \n",
    " ⇂ alter table `participants` add constraint `participants_user_id_foreign` foreign key (`user_id`) references `users` (`id`) on delete cascade on update cascade \n",
    " ⇂ alter table `participants` add index `participants_conversation_id_index`(`conversation_id`) \n",
    " ⇂ alter table `private_messages` add `conversation_id` int unsigned null after `id` \n",
    " ⇂ select `id`, `sender_id`, `receiver_id`, `subject`, `read`, `related_to`, `created_at`, `updated_at` from `private_messages` where `id` is not null order by `id` desc limit 1000 \n",
    " ⇂ update `conversations` inner join (select `conversation_id`, MIN(created_at) AS min_created_at from `private_messages` group by `conversation_id`) as `oldest_messages` on `oldest_messages`.`conversation_id` = `conversations`.`id` set `conversations`.`created_at` = oldest_messages.min_created_at \n",
    " ⇂ update `participants` inner join `conversations` on `conversations`.`id` = `participants`.`conversation_id` set `participants`.`created_at` = conversations.created_at \n",
    " ⇂ alter table `private_messages` modify `conversation_id` int unsigned not null \n",
    " ⇂ alter table `private_messages` drop foreign key `private_messages_receiver_id_foreign` \n",
    " ⇂ alter table `private_messages` drop `receiver_id`, drop `subject`, drop `related_to`, drop `read` \n",
    " ⇂ alter table `private_messages` add constraint `private_messages_conversation_id_foreign` foreign key (`conversation_id`) references `conversations` (`id`) on delete cascade on update cascade \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 10 to 13): echo "plain text" before Schema::table.
const B1B_ECHO_BEFORE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_02_000001_e_echo_before ............................................  \n",
    "plain text\n",
    "  ⇂ alter table `orders` add `s_a` int unsigned null  \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 10 to 13): echo between two Schema::table calls.
const B1B_ECHO_BETWEEN_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_02_000002_e_echo_between ...........................................  \n",
    "plain text\n",
    "  ⇂ alter table `orders` add `s_a` int unsigned null  \n",
    "  ⇂ alter table `orders` add `s_b` int unsigned null  \n",
    "\n",
);

/// Laravel 13.40.0, the short form: echo between two Schema::table calls.
const B1B_ECHO_BETWEEN_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2025_06_02_000002_e_echo_between .. \n",
    "plain text\n",
    " ⇂ alter table `orders` add `s_a` int unsigned null \n",
    " ⇂ alter table `orders` add `s_b` int unsigned null \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 10 to 13): echo after the last Schema::table.
const B1B_ECHO_AFTER_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_02_000003_e_echo_after .............................................  \n",
    "plain text\n",
    "  ⇂ alter table `orders` add `s_a` int unsigned null  \n",
    "  ⇂ alter table `orders` add `s_b` int unsigned null  \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 10 to 13): a migration that only prints text.
const B1B_ECHO_ONLY_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_02_000005_e_echo_only ..............................................  \n",
    "plain text only\n",
    "\n",
    "\n",
);

/// Laravel 13.40.0, the short form: a migration that only prints text.
const B1B_ECHO_ONLY_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2025_06_02_000005_e_echo_only .. \n",
    "plain text only\n",
    "\n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 10 to 13): a tab-indented line and no statement.
const B1B_UNIT3D_ONLY_OUTPUT_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_02_000012_e_unit3d_only_output .....................................  \n",
    "\tOnly text, no statement.\n",
    "\n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 10 to 13): text printed without a line break runs into the first statement line.
const B1B_ECHO_NONEWLINE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_02_000004_e_echo_nonewline .........................................  \n",
    "no newline  ⇂ alter table `orders` add `s_a` int unsigned null  \n",
    "  ⇂ alter table `orders` add `s_b` int unsigned null  \n",
    "\n",
);

/// Laravel 13.40.0, the short form: text printed without a line break.
const B1B_ECHO_NONEWLINE_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2025_06_02_000004_e_echo_nonewline .. \n",
    "no newline ⇂ alter table `orders` add `s_a` int unsigned null \n",
    " ⇂ alter table `orders` add `s_b` int unsigned null \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 10 to 13): printed text with a color escape, a tab and indentation.
const B1B_ECHO_BLANK_AND_ANSI_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_02_000013_e_echo_blank_and_ansi ....................................  \n",
    "\n",
    "\u{1b}[32mgreen text\u{1b}[0m\n",
    "\ttabbed text\n",
    "  indented text\n",
    "  ⇂ alter table `orders` add `s_a` int unsigned null  \n",
    "  ⇂ alter table `orders` add `s_b` int unsigned null  \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 10 to 13): dump() output.
const B1B_DUMP_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_02_000006_e_dump ...................................................  \n",
    "\"x\" // database/migrations_b1b/2025_06_02_000006_e_dump.php:14\n",
    "string(1) \"y\"\n",
    "  ⇂ alter table `orders` add `s_a` int unsigned null  \n",
    "  ⇂ alter table `orders` add `s_b` int unsigned null  \n",
    "\n",
);

/// Laravel 13.40.0, the short form: dump() output.
const B1B_DUMP_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2025_06_02_000006_e_dump .. \n",
    "\"x\" // database/migrations_b1b/2025_06_02_000006_e_dump.php:14\n",
    "string(1) \"y\"\n",
    " ⇂ alter table `orders` add `s_a` int unsigned null \n",
    " ⇂ alter table `orders` add `s_b` int unsigned null \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 10 to 13): what reaches standard output from echo, print and printf.
const B1B_STDERR_PRINT_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_02_000010_e_stderr_print ...........................................  \n",
    "to STDOUT\n",
    "print text\n",
    "printf text\n",
    "  ⇂ alter table `orders` add `s_a` int unsigned null  \n",
    "  ⇂ alter table `orders` add `s_b` int unsigned null  \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 10 to 13): a migration prints a line that starts with ALTER TABLE.
const B1B_SQL_WORD_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_02_000008_e_sql_word ...............................................  \n",
    "ALTER TABLE x ADD y INT;\n",
    "  ⇂ alter table `orders` add `s_a` int unsigned null  \n",
    "  ⇂ alter table `orders` add `s_b` int unsigned null  \n",
    "\n",
);

/// Laravel 13.40.0, the short form: a printed line that starts with ALTER TABLE.
const B1B_SQL_WORD_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2025_06_02_000008_e_sql_word .. \n",
    "ALTER TABLE x ADD y INT;\n",
    " ⇂ alter table `orders` add `s_a` int unsigned null \n",
    " ⇂ alter table `orders` add `s_b` int unsigned null \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 10 to 13): a printed line that starts with create table, and a statement named fake.
const B1B_SQL_WORD_BEFORE_AND_AFTER_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_02_000009_e_sql_word_before_and_after ..............................  \n",
    "create table z (id int)\n",
    "  ⇂ alter table `orders` add `fake` int\n",
    "  ⇂ alter table `orders` add `s_a` int unsigned null  \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 11 and 13.40.0): text, json and blob columns with a default.
const B1B_TABLE_DEFAULTS_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_01_000001_d_table_defaults .........................................  \n",
    "  ⇂ alter table `orders` add `a` text not null default ''  \n",
    "  ⇂ alter table `orders` add `b` text null default 'x'  \n",
    "  ⇂ alter table `orders` add `c` json null default '[]'  \n",
    "  ⇂ alter table `orders` add `d` json not null default ('[]')  \n",
    "  ⇂ alter table `orders` modify `note` text not null default ''  \n",
    "  ⇂ alter table `orders` add `e` blob not null default ''  \n",
    "  ⇂ alter table `orders` add `f` longtext not null default ''  \n",
    "\n",
);

/// Laravel 13.40.0, the short form: text, json and blob columns with a default.
const B1B_TABLE_DEFAULTS_SHORT: &str = concat!(
    "\n",
    " INFO Running migrations. \n",
    "\n",
    " 2025_06_01_000001_d_table_defaults .. \n",
    " ⇂ alter table `orders` add `a` text not null default '' \n",
    " ⇂ alter table `orders` add `b` text null default 'x' \n",
    " ⇂ alter table `orders` add `c` json null default '[]' \n",
    " ⇂ alter table `orders` add `d` json not null default ('[]') \n",
    " ⇂ alter table `orders` modify `note` text not null default '' \n",
    " ⇂ alter table `orders` add `e` blob not null default '' \n",
    " ⇂ alter table `orders` add `f` longtext not null default '' \n",
    "\n",
);

/// Laravel 10.50.4, derived from the framework source and not run: Laravel 10 joins the columns of one Schema::table into one ALTER TABLE.
const B1B_TABLE_DEFAULTS_L10: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_01_000008_d_table_defaults_nochange ................................  \n",
    "  ⇂ alter table `orders` add `a` text not null default '', add `b` text null default 'x', add `c` json null default '[]', add `d` json not null default ('[]'), add `e` blob not null default '', add `f` longtext not null default ''  \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 11 and 13.40.0): ->change() to text not null default ''.
const B1B_NOTE_CHANGE_ONLY_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_01_000007_d_note_change_only .......................................  \n",
    "  ⇂ alter table `orders` modify `note` text not null default ''  \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 11 and 13.40.0): expression defaults and a quoted json default.
const B1B_EXPR_JSON_ARRAY_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_01_000006_d_expr_json_array ........................................  \n",
    "  ⇂ alter table `orders` add `j5` json not null default (JSON_ARRAY())  \n",
    "  ⇂ alter table `orders` add `t6` text not null default ('')  \n",
    "  ⇂ alter table `orders` add `j7` json not null default '[]'  \n",
    "\n",
);

/// Laravel 12.71.0 (the same bytes in 13.40.0): a text default with a quote and a backslash.
const B1B_QUOTE_LONG: &str = concat!(
    "\n",
    "   INFO  Running migrations.  \n",
    "\n",
    "  2025_06_01_000009_d_quote ..................................................  \n",
    "  ⇂ alter table `orders` add `q1` text not null default 'it''s'  \n",
    "  ⇂ alter table `orders` add `q2` text not null default 'a\\b'  \n",
    "\n",
);

/// `orders` as a mysqldump schema writes it, with a VARCHAR `note` that a
/// migration changes into text.
const B1B_ORDERS_WITH_NOTE: &str = "create table `orders` (`id` bigint unsigned not null auto_increment, \
    `note` varchar(50) default null, primary key (`id`)) \
    engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n";

const B1B_FAILS_1101: &str = "fails with error 1101";

/// What kilit says about every statement of `output` on both servers, with
/// the tables of `schema`: the same on both, or the test fails.
fn b1b_answers(schema: &str, output: &str) -> Vec<String> {
    let on_80 = answers("mysql-8.0.46", schema, output);
    let on_84 = answers("mysql-8.4.13", schema, output);
    assert_eq!(on_80, on_84, "{output}");
    on_84
}

#[test]
fn laravel_12_and_13_text_json_and_blob_defaults_fail_with_1101_where_mysql_refuses_them() {
    // $table->text('a')->default(''); $table->text('b')->nullable()->default('x');
    // $table->json('c')->nullable()->default('[]');
    // $table->json('d')->default(new Expression("('[]')"));
    // $table->text('note')->default('')->change();
    // $table->binary('e')->default(''); $table->longText('f')->default('');
    for (form, output) in [
        ("Laravel 12 and 13, long form", B1B_TABLE_DEFAULTS_LONG),
        ("Laravel 13, short form", B1B_TABLE_DEFAULTS_SHORT),
    ] {
        for (tables, schema) in [("no table", ""), ("the table", B1B_ORDERS_WITH_NOTE)] {
            // The expression default is a copy on a table
            // kilit knows and unknown on one it does not.
            let expression = if schema.is_empty() { "unknown" } else { COPY };
            assert_eq!(
                b1b_answers(schema, output),
                [
                    B1B_FAILS_1101, // a text not null default ''
                    B1B_FAILS_1101, // b text null default 'x'
                    B1B_FAILS_1101, // c json null default '[]'
                    expression,     // d json not null default ('[]'): an expression
                    B1B_FAILS_1101, // modify note text not null default ''
                    B1B_FAILS_1101, // e blob not null default ''
                    B1B_FAILS_1101, // f longtext not null default ''
                ],
                "{form}, {tables}"
            );
        }
    }
}

#[test]
fn laravel_changing_a_column_to_text_with_a_default_fails_with_1101_with_or_without_the_table() {
    // $table->text('note')->default('')->change(); in Laravel 11 to 13
    // (Laravel 10 goes through Doctrine, which prints other SQL).
    for schema in ["", B1B_ORDERS_WITH_NOTE] {
        assert_eq!(
            b1b_answers(schema, B1B_NOTE_CHANGE_ONLY_LONG),
            [B1B_FAILS_1101],
            "{schema}"
        );
    }
}

#[test]
fn laravel_json_defaults_written_as_a_literal_fail_and_as_an_expression_copy() {
    // $table->json('j7')->default('[]'); the expression form is
    // default(new Expression("('')")) on text.
    let answers = b1b_answers("", B1B_EXPR_JSON_ARRAY_LONG);
    assert_eq!(answers.len(), 3, "{answers:?}");
    // j5 json not null default (JSON_ARRAY()): not an answer of MySQL's
    // success list.
    assert_ne!(answers[0], INSTANT);
    // A copy only on a plain table, and there is none here.
    assert_eq!(answers[1], "unknown", "t6 text not null default ('')");
    assert_eq!(answers[2], B1B_FAILS_1101, "j7 json not null default '[]'");
}

#[test]
fn laravel_text_defaults_with_a_quote_or_a_backslash_fail_with_1101() {
    // $table->text('q1')->default("it's"); $table->text('q2')->default('a\b');
    // Laravel 12 and 13 double the quote.
    assert_eq!(
        b1b_answers("", B1B_QUOTE_LONG),
        [B1B_FAILS_1101, B1B_FAILS_1101]
    );
}

#[test]
fn laravel_10_joins_the_columns_of_a_table_into_one_alter_and_that_is_never_called_safe() {
    // Derived, not run: the six columns of the migration are one statement.
    // MySQL refuses it with 1101 at the first text column; kilit says that or
    // that it cannot tell, and never a success.
    for schema in ["", B1B_ORDERS_WITH_NOTE] {
        let answers = b1b_answers(schema, B1B_TABLE_DEFAULTS_L10);
        assert_eq!(answers.len(), 1, "{answers:?}");
        assert!(
            answers[0] == B1B_FAILS_1101 || answers[0] == "unknown",
            "{answers:?}"
        );
    }
}

/// The text of the lines a migration printed, as kilit keeps it apart from
/// the statements: the line number, and the text without the white space
/// around it.
fn b1b_printed(migration: &LaravelMigration) -> Vec<(usize, String)> {
    migration
        .printed
        .iter()
        .map(|line| (line.line, line.text.trim().to_owned()))
        .collect()
}

/// The lines of the output that kilit could not read, as (line, text with
/// the white space around it removed).
fn b1b_unreadable(pretend: &Pretend) -> Vec<(usize, String)> {
    pretend
        .unreadable
        .iter()
        .map(|line| (line.line, line.text.trim().to_owned()))
        .collect()
}

fn b1b_read(output: &str) -> Pretend {
    read_pretend(output).expect("the output of migrate --pretend")
}

/// The SQL of the statements of the only migration of `pretend`.
fn b1b_sql(pretend: &Pretend) -> Vec<String> {
    assert_eq!(pretend.migrations.len(), 1, "{:?}", pretend.migrations);
    pretend.migrations[0]
        .statements
        .iter()
        .map(|statement| statement.sql.clone())
        .collect()
}

#[test]
fn what_unit3d_s_migration_prints_between_its_name_and_its_statements_is_text_not_an_unreadable_line()
 {
    // A real migration (unit3d, create_private_message_conversations) that
    // tells the user what it does with a tab-indented line per step.
    let expected = [
        (
            6,
            "This migration is long and will take approximately 30-60 seconds per 100 thousand stored private messages.",
        ),
        (7, "Creating new tables and columns..."),
        (8, "Parsing previous private message structure..."),
        (
            9,
            "Inserting all conversations into new conversations table...",
        ),
        (10, "Adding conversation_id to all private messages..."),
        (11, "Inserting all conversation participants..."),
        (12, "Updating all conversation timestamps..."),
        (13, "Updating all participant timestamps..."),
        (
            14,
            "Dropping unused columns from private messages and adding constraints...",
        ),
    ];
    for (form, output) in [
        ("Laravel 12 and 13, long form", B1B_UNIT3D_LONG),
        ("Laravel 13, short form", B1B_UNIT3D_SHORT),
    ] {
        let pretend = b1b_read(output);
        assert!(
            pretend.unreadable.is_empty(),
            "{form}: {:?}",
            pretend.unreadable
        );
        assert_eq!(pretend.error, None, "{form}");
        assert_eq!(pretend.migrations.len(), 1, "{form}");
        let migration = &pretend.migrations[0];
        assert_eq!(
            migration.name, "2024_06_06_042258_create_private_message_conversations",
            "{form}"
        );
        assert_eq!(migration.statements.len(), 17, "{form}");
        let expected: Vec<(usize, String)> = expected
            .iter()
            .map(|(line, text)| (*line, (*text).to_owned()))
            .collect();
        assert_eq!(b1b_printed(migration), expected, "{form}");
    }
}

#[test]
fn text_printed_before_the_statements_of_a_migration_is_kept_apart_in_every_place_it_can_sit() {
    // echo "plain text" before, between and after the Schema::table calls:
    // Laravel prints the statements after up() returns, so the text always
    // comes first.
    for (form, output, statements) in [
        ("echo before", B1B_ECHO_BEFORE_LONG, 1),
        ("echo between", B1B_ECHO_BETWEEN_LONG, 2),
        ("echo between, short form", B1B_ECHO_BETWEEN_SHORT, 2),
        ("echo after", B1B_ECHO_AFTER_LONG, 2),
    ] {
        let pretend = b1b_read(output);
        assert!(
            pretend.unreadable.is_empty(),
            "{form}: {:?}",
            pretend.unreadable
        );
        let sql = b1b_sql(&pretend);
        assert_eq!(sql.len(), statements, "{form}: {sql:?}");
        assert_eq!(
            sql[0], "alter table `orders` add `s_a` int unsigned null",
            "{form}"
        );
        assert_eq!(
            b1b_printed(&pretend.migrations[0]),
            [(5, "plain text".to_owned())],
            "{form}"
        );
    }
}

#[test]
fn dump_output_and_the_output_of_print_and_printf_are_text_of_the_migration() {
    let pretend = b1b_read(B1B_DUMP_LONG);
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    assert_eq!(
        b1b_printed(&pretend.migrations[0]),
        [
            (
                5,
                "\"x\" // database/migrations_b1b/2025_06_02_000006_e_dump.php:14".to_owned()
            ),
            (6, "string(1) \"y\"".to_owned()),
        ]
    );
    assert_eq!(b1b_sql(&pretend).len(), 2);

    let pretend = b1b_read(B1B_DUMP_SHORT);
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    assert_eq!(pretend.migrations[0].printed.len(), 2);

    let pretend = b1b_read(B1B_STDERR_PRINT_LONG);
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    assert_eq!(
        b1b_printed(&pretend.migrations[0]),
        [
            (5, "to STDOUT".to_owned()),
            (6, "print text".to_owned()),
            (7, "printf text".to_owned()),
        ]
    );
    assert_eq!(b1b_sql(&pretend).len(), 2);
}

#[test]
fn printed_text_with_a_color_a_tab_and_indentation_is_text_of_the_migration() {
    // The migration prints a blank line, a line in green, a tabbed line and
    // an indented one.
    let pretend = b1b_read(B1B_ECHO_BLANK_AND_ANSI_LONG);
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    let printed = b1b_printed(&pretend.migrations[0]);
    assert_eq!(printed.len(), 3, "{printed:?}");
    assert!(printed[0].1.contains("green text"), "{printed:?}");
    assert!(printed[1].1.contains("tabbed text"), "{printed:?}");
    assert!(printed[2].1.contains("indented text"), "{printed:?}");
    assert_eq!(b1b_sql(&pretend).len(), 2);
}

#[test]
fn text_from_a_migration_with_no_statement_is_text_when_another_migration_follows() {
    // A migration that only echoes, then the next migration's name line:
    // Laravel went on, so the text is the migration's.
    let output = "
   INFO  Running migrations.

  2025_06_02_000005_e_echo_only ..............................................
plain text only

  2025_06_02_000011_e_unit3d_form ............................................
  \u{21c2} alter table `orders` add `s_a` int unsigned null
";
    let pretend = b1b_read(output);
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    assert_eq!(pretend.migrations.len(), 2);
    assert!(pretend.migrations[0].statements.is_empty());
    assert_eq!(
        b1b_printed(&pretend.migrations[0]),
        [(5, "plain text only".to_owned())]
    );
    assert!(pretend.migrations[1].printed.is_empty());
}

#[test]
fn text_at_the_very_end_of_the_output_is_an_unreadable_line() {
    // Real runs of a migration that only prints, as the last one: an error
    // screen kilit does not recognize also ends the output, so the text is
    // never taken for the migration's. (Laravel exits 0 on these; kilit
    // cannot tell them from an error.)
    for (form, output, expected) in [
        ("echo only", B1B_ECHO_ONLY_LONG, "plain text only"),
        (
            "echo only, short form",
            B1B_ECHO_ONLY_SHORT,
            "plain text only",
        ),
        (
            "tabbed line only",
            B1B_UNIT3D_ONLY_OUTPUT_LONG,
            "Only text, no statement.",
        ),
    ] {
        let pretend = b1b_read(output);
        assert_eq!(
            b1b_unreadable(&pretend),
            [(5, expected.to_owned())],
            "{form}"
        );
        assert!(pretend.migrations[0].printed.is_empty(), "{form}");
        assert!(pretend.migrations[0].statements.is_empty(), "{form}");
    }
}

#[test]
fn a_symfony_style_file_and_line_header_is_never_printed_text() {
    let output = "
   INFO  Running migrations.

  2026_02_01_000041_first_alter_orders_table .................................
In 2026_02_01_000041_first_alter_orders_table.php line 15:
  \u{21c2} alter table `orders` add `qty` int null
";
    let pretend = b1b_read(output);
    assert!(pretend.migrations[0].printed.is_empty());
}

#[test]
fn a_printed_line_that_starts_with_an_sql_word_is_an_unreadable_line_and_nothing_is_skipped() {
    // echo "ALTER TABLE x ADD y INT;": the user may have printed a statement
    // instead of running it; kilit does not skip it.
    for (form, output) in [
        ("long form", B1B_SQL_WORD_LONG),
        ("short form", B1B_SQL_WORD_SHORT),
    ] {
        let pretend = b1b_read(output);
        assert_eq!(
            b1b_unreadable(&pretend),
            [(5, "ALTER TABLE x ADD y INT;".to_owned())],
            "{form}"
        );
        assert!(pretend.migrations[0].printed.is_empty(), "{form}");
        // The two real statements are still read.
        assert_eq!(b1b_sql(&pretend).len(), 2, "{form}");
    }
    let pretend = b1b_read(B1B_SQL_WORD_BEFORE_AND_AFTER_LONG);
    assert_eq!(
        b1b_unreadable(&pretend),
        [(5, "create table z (id int)".to_owned())]
    );
    assert!(pretend.migrations[0].printed.is_empty());
    assert_eq!(
        b1b_sql(&pretend),
        [
            "alter table `orders` add `fake` int",
            "alter table `orders` add `s_a` int unsigned null"
        ]
    );
}

#[test]
fn text_printed_without_a_line_break_that_runs_into_a_statement_line_is_unreadable() {
    // echo "no newline": the text and Laravel's mark share a line, so the
    // statement on it is not read. kilit must not take the line for text.
    for (form, output) in [
        ("long form", B1B_ECHO_NONEWLINE_LONG),
        ("short form", B1B_ECHO_NONEWLINE_SHORT),
    ] {
        let pretend = b1b_read(output);
        assert_eq!(
            pretend.unreadable.len(),
            1,
            "{form}: {:?}",
            pretend.unreadable
        );
        assert_eq!(pretend.unreadable[0].line, 5, "{form}");
        let text = &pretend.unreadable[0].text;
        assert!(text.contains("no newline"), "{form}: {text}");
        assert!(
            text.contains("alter table `orders` add `s_a` int unsigned null"),
            "{form}: {text}"
        );
        assert!(pretend.migrations[0].printed.is_empty(), "{form}");
        // The statement on the next line is read.
        assert_eq!(
            b1b_sql(&pretend),
            ["alter table `orders` add `s_b` int unsigned null"],
            "{form}"
        );
    }
}

#[test]
fn a_laravel_10_dump_error_line_after_a_migration_name_stays_unreadable() {
    // Laravel 10 prints `ERROR  [name] failed to dump queries.` when it
    // cannot dump a migration's queries. It is Laravel's own line: neither the
    // migration's text nor skipped.
    let output = "
   INFO  Running migrations.

  2026_11_02_090100_widen_amount_on_orders_table .............................

   ERROR  [2026_11_02_090100_widen_amount_on_orders_table] failed to dump queries.
  \u{21c2} alter table `orders` add `qty` int null
";
    for output in [
        output.to_owned(),
        output.replace("  \u{21c2} alter table `orders` add `qty` int null\n", ""),
    ] {
        let pretend = b1b_read(&output);
        assert_eq!(
            b1b_unreadable(&pretend),
            [(
                6,
                "ERROR  [2026_11_02_090100_widen_amount_on_orders_table] failed to dump queries."
                    .to_owned()
            )]
        );
        assert!(pretend.migrations[0].printed.is_empty());
    }
}

#[test]
fn a_printed_sql_statement_that_looks_like_laravel_s_dotted_line_is_unreadable() {
    // A line with " .." in it was skipped as Laravel's own dotted task line
    // before, statement or not.
    for line in [
        "ALTER TABLE x ADD y INT ..",
        "alter table `x` add `y` int ..",
        "  create table z (id int) ..",
        "DROP TABLE x ..",
    ] {
        let output = format!(
            "
   INFO  Running migrations.

  2026_11_02_090100_widen_amount_on_orders_table .............................
{line}
  \u{21c2} alter table `orders` add `qty` int null
"
        );
        let pretend = b1b_read(&output);
        assert_eq!(
            pretend.unreadable.len(),
            1,
            "{line}: {:?}",
            pretend.unreadable
        );
        assert_eq!(pretend.unreadable[0].line, 5, "{line}");
        assert_eq!(pretend.unreadable[0].text.trim(), line.trim(), "{line}");
        assert!(pretend.migrations[0].printed.is_empty(), "{line}");
    }
}

#[test]
fn printed_text_belongs_to_the_migration_that_printed_it() {
    // Two migrations: the first prints nothing, the second prints a line. And
    // the other way round, with the text of the first followed by the second's
    // name line.
    let first_plain = "
   INFO  Running migrations.

  2026_11_02_090000_add_qty_to_orders_table ..................................
  \u{21c2} alter table `orders` add `qty` int null
  2026_11_02_090100_widen_amount_on_orders_table .............................
plain text
  \u{21c2} alter table `orders` modify `amount` bigint not null
";
    let pretend = b1b_read(first_plain);
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    assert_eq!(pretend.migrations.len(), 2);
    assert!(pretend.migrations[0].printed.is_empty());
    assert_eq!(
        b1b_printed(&pretend.migrations[1]),
        [(7, "plain text".to_owned())]
    );

    let second_plain = "
   INFO  Running migrations.

  2026_11_02_090000_add_qty_to_orders_table ..................................
plain text
  \u{21c2} alter table `orders` add `qty` int null
  2026_11_02_090100_widen_amount_on_orders_table .............................
  \u{21c2} alter table `orders` modify `amount` bigint not null
";
    let pretend = b1b_read(second_plain);
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    assert_eq!(
        b1b_printed(&pretend.migrations[0]),
        [(5, "plain text".to_owned())]
    );
    assert!(pretend.migrations[1].printed.is_empty());
}

#[test]
fn text_a_migration_prints_does_not_change_what_kilit_reads_of_its_statements() {
    // unit3d's migration without its nine lines gives the same statements.
    let without_text: String = B1B_UNIT3D_LONG
        .lines()
        .filter(|line| !line.starts_with('\t'))
        .map(|line| format!("{line}\n"))
        .collect();
    let with = b1b_read(B1B_UNIT3D_LONG);
    let without = b1b_read(&without_text);
    assert!(without.unreadable.is_empty(), "{:?}", without.unreadable);
    assert_eq!(b1b_sql(&with), b1b_sql(&without));
    assert!(without.migrations[0].printed.is_empty());
}

// ---------------------------------------------------------------------------
// Step B1b, fix phase: a statement line whose mark a code page has mangled.
// Windows passes Laravel's mark `⇂` (the bytes E2 87 82) on as the characters
// of the console's code page; in code page 936 they read `鈬�`. The line is
// still a statement, so it must not be taken for text a migration printed.
// The lines are built from the bytes of Laravel 12.71.0's output with the
// mark replaced by what each code page makes of it.

/// A long-form output whose first migration has `line` between its name and
/// its first known statement.
fn mangled_before_a_statement(line: &str) -> String {
    format!(
        "\n   INFO  Running migrations.  \n\n  2025_06_02_000020_g_mangled_mark ...........................................  \n{line}\n  ⇂ alter table `orders` add `s_a` int unsigned null  \n\n"
    )
}

/// The same with the short form's spacing.
fn mangled_before_a_statement_short(line: &str) -> String {
    format!(
        "\n INFO Running migrations. \n\n 2025_06_02_000020_g_mangled_mark .. \n{line}\n ⇂ alter table `orders` add `s_a` int unsigned null \n\n"
    )
}

/// Marks and statements a code page can make of one statement line.
const MANGLED_STATEMENT_LINES: [&str; 9] = [
    // Code page 936.
    "  鈬� alter table `orders` add fulltext `orders_note_fulltext`(`note`)  ",
    "  鈬� create index `orders_amount_index` on `orders` (`amount`)  ",
    // A mark of a single letter-led token.
    "  x alter table `orders` add `b` int null  ",
    "  ab drop index `orders_amount_index`  ",
    "  Z insert into `orders` (`amount`) values (1)  ",
    // A mark of two tokens.
    "  â ‡ alter table `orders` add `c` int null  ",
    "  a b create index `i` on `orders` (`note`)  ",
    // An SQL word followed by a backticked name, far from the line's start.
    "  a b c update `orders` set `amount` = 1  ",
    "  é\u{2021}\u{201a} delete from `orders`  ",
];

#[test]
fn a_statement_line_with_a_mark_kilit_does_not_know_stays_an_unreadable_line() {
    for line in MANGLED_STATEMENT_LINES {
        for (form, output) in [
            ("long form", mangled_before_a_statement(line)),
            ("short form", mangled_before_a_statement_short(line)),
        ] {
            let pretend = b1b_read(&output);
            assert_eq!(
                b1b_unreadable(&pretend),
                [(5, line.trim().to_owned())],
                "{form}: {line}"
            );
            assert!(
                pretend.migrations[0].printed.is_empty(),
                "{form}: {line}: {:?}",
                pretend.migrations[0].printed
            );
            // Laravel's own statement after it is still read.
            assert_eq!(
                b1b_sql(&pretend),
                ["alter table `orders` add `s_a` int unsigned null"],
                "{form}: {line}"
            );
        }
    }
}

#[test]
fn a_statement_line_with_an_unknown_mark_before_the_next_migration_stays_an_unreadable_line() {
    // The first migration has no statement a known mark shows; the next
    // migration's name ends what it printed.
    for line in MANGLED_STATEMENT_LINES {
        let output = format!(
            "\n   INFO  Running migrations.  \n\n  2025_06_02_000021_g_first ..................................................  \n{line}\n  2025_06_02_000022_g_second .................................................  \n  ⇂ alter table `orders` add `s_a` int unsigned null  \n\n"
        );
        let pretend = b1b_read(&output);
        assert_eq!(
            b1b_unreadable(&pretend),
            [(5, line.trim().to_owned())],
            "{line}"
        );
        assert_eq!(pretend.migrations.len(), 2, "{line}");
        assert!(pretend.migrations[0].printed.is_empty(), "{line}");
        assert_eq!(
            pretend.migrations[1].statements.len(),
            1,
            "{line}: {:?}",
            pretend.migrations[1].statements
        );
    }
}

#[test]
fn a_mangled_statement_line_between_printed_text_and_the_statements_is_unreadable_and_the_text_is_not()
 {
    // Text the migration printed, then a statement line with a mangled mark.
    let output = "
   INFO  Running migrations.

  2025_06_02_000023_g_text_and_mark ..........................................
\tCreating new tables and columns...
  鈬� alter table `orders` add fulltext `orders_note_fulltext`(`note`)
  ⇂ alter table `orders` add `s_a` int unsigned null

";
    let pretend = b1b_read(output);
    assert_eq!(
        b1b_unreadable(&pretend),
        [(
            6,
            "鈬� alter table `orders` add fulltext `orders_note_fulltext`(`note`)".to_owned()
        )]
    );
    assert_eq!(
        b1b_sql(&pretend),
        ["alter table `orders` add `s_a` int unsigned null"]
    );
}

#[test]
fn unit3d_s_lines_with_words_such_as_table_and_columns_stay_printed_text() {
    // unit3d's lines, which hold words such as "table" and "columns" but
    // start no statement, are still text.
    let pretend = b1b_read(B1B_UNIT3D_LONG);
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    assert_eq!(pretend.migrations[0].printed.len(), 9);
}

// ---------------------------------------------------------------------------
// Step B1b, second fix phase: text a migration prints without a line break
// runs into the statement line that follows it. When the console's code
// page has mangled that line's mark, the line is a statement kilit cannot
// read (it may change data), not text. The inputs are Laravel outputs as a
// Windows console in code page 1252 shows them (the mark `⇂` as `â‡‚`). By
// hand on MySQL 8.0.46 and 8.4.13, `insert ignore` filled the table and the ADD UNIQUE after it
// gave 1062, so the output must not be taken as fully read.

/// Statement lines with the text a migration printed before them.
const RUN_INTO_LINES: [&str; 7] = [
    "no newline  â‡‚ SET FOREIGN_KEY_CHECKS=0;",
    "Copying codes  â‡‚ insert ignore into `t` (`code`) values ('a'), ('a')",
    "Done  â‡‚ update orders set x = 1",
    "Done  â‡‚ alter table `orders` add `b` int null",
    // A mark that came through as a question mark.
    "Copying codes  ? insert ignore into `t` (`code`) values ('a')",
    // A mark of a double-byte code page.
    "Copying codes  鈬� delete from `t`",
    // A mark of one non-ASCII token.
    "Copying codes  é‡ truncate table `t`",
];

#[test]
fn text_printed_without_a_line_break_before_a_mangled_statement_line_is_an_unreadable_line() {
    for line in RUN_INTO_LINES {
        for (form, output) in [
            ("long form", mangled_before_a_statement(line)),
            ("short form", mangled_before_a_statement_short(line)),
        ] {
            let pretend = b1b_read(&output);
            assert_eq!(
                b1b_unreadable(&pretend),
                [(5, line.trim().to_owned())],
                "{form}: {line}"
            );
            assert!(
                pretend.migrations[0].printed.is_empty(),
                "{form}: {line}: {:?}",
                pretend.migrations[0].printed
            );
            assert_eq!(
                b1b_sql(&pretend),
                ["alter table `orders` add `s_a` int unsigned null"],
                "{form}: {line}"
            );
        }
    }
}

#[test]
fn a_run_in_output_keeps_its_insert_ignore_as_an_unreadable_line() {
    // Such an output with Laravel's own lines in the right mark: a migration that
    // prints "Copying codes" without a line break and then runs
    // `insert ignore`, before an ADD UNIQUE.
    let output = "
   INFO  Running migrations.

  2025_06_02_000001_create_t ..............
  ⇂ create table `t` (`id` bigint unsigned not null auto_increment primary key, `code` varchar(20) not null)
  2025_06_02_000002_fill_t ..............
Copying codes  â‡‚ insert ignore into `t` (`code`) values ('a'), ('a')
  ⇂ alter table `t` add unique `t_code_unique`(`code`)

";
    let pretend = b1b_read(output);
    assert_eq!(
        b1b_unreadable(&pretend),
        [(
            7,
            "Copying codes  â‡‚ insert ignore into `t` (`code`) values ('a'), ('a')".to_owned()
        )]
    );
    assert!(pretend.migrations[1].printed.is_empty());
    assert_eq!(pretend.migrations[1].statements.len(), 1);
}

#[test]
fn plain_text_printed_before_a_statement_line_stays_text() {
    // Regression: a printed line with no mark and no statement word (as in
    // unit3d's nine lines) is still text.
    let output = mangled_before_a_statement("\tCreating new tables and columns...");
    let pretend = b1b_read(&output);
    assert!(pretend.unreadable.is_empty(), "{:?}", pretend.unreadable);
    assert_eq!(pretend.migrations[0].printed.len(), 1);
}

// ---------------------------------------------------------------------------
// Step B1c: no measured answer on a table kilit does not know.
//
// The statements are what Laravel 10 to 13 print for the call named in each
// comment (the MySQL grammar is the same in all four majors for these); the
// expectations come from the lab, from hand tests on MySQL 8.0.46 and 8.4.13
// and from the decisions of the step.

/// `orders` as mysqldump writes it, with a plain and a FULLTEXT index: a
/// column added to it is a copy.
const B1C_ORDERS_WITH_FULLTEXT: &str = "create table `orders` (\
    `id` bigint unsigned not null auto_increment, `note` text, \
    `amount` int not null default '0', primary key (`id`), \
    key `ix_amount` (`amount`), fulltext key `ft_note` (`note`)) \
    engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n";

const B1C_NAME: &str = "2026_11_11_100000_change_orders_table";

#[test]
fn laravel_columns_and_indexes_on_a_table_without_a_schema_are_unknown_in_both_forms() {
    // $table->string('coupon')->nullable(); $table->integer('qty');
    // $table->dropColumn('legacy_flag'); $table->renameColumn('note', 'memo');
    // $table->index('amount'); $table->integer('qty')->default(0)
    let statements = [
        "alter table `orders` add `coupon` varchar(255) null",
        "alter table `orders` add `qty` int not null",
        "alter table `orders` drop `legacy_flag`",
        "alter table `orders` rename column `note` to `memo`",
        "alter table `orders` add index `orders_amount_index`(`amount`)",
    ];
    for (form, output) in [
        ("long", long_form(&[(B1C_NAME, &statements)])),
        ("short", short_form(&[(B1C_NAME, &statements)])),
    ] {
        assert_eq!(b1b_answers("", &output), ["unknown"; 5], "{form}");
    }
}

#[test]
fn the_same_laravel_statements_keep_the_lab_answers_on_a_table_the_schema_has() {
    // Regression: with the table the answers are the lab's (the index is on the
    // column the first statement adds).
    let statements = [
        "alter table `orders` add `coupon` varchar(255) null",
        "alter table `orders` add index `orders_coupon_index`(`coupon`)",
    ];
    for (form, output) in [
        ("long", long_form(&[(B1C_NAME, &statements)])),
        ("short", short_form(&[(B1C_NAME, &statements)])),
    ] {
        assert_eq!(b1b_answers(ORDERS, &output), [INSTANT, INPLACE], "{form}");
    }
}

#[test]
fn laravel_renaming_an_index_keeps_the_table_known_for_the_next_statement() {
    // $table->renameIndex('ix_amount', 'ix_total'); then
    // $table->string('coupon')->nullable(); on a table with a FULLTEXT index.
    let statements = [
        "alter table `orders` rename index `ix_amount` to `ix_total`",
        "alter table `orders` add `coupon` varchar(255) null",
    ];
    for (form, output) in [
        ("long", long_form(&[(B1C_NAME, &statements)])),
        ("short", short_form(&[(B1C_NAME, &statements)])),
    ] {
        assert_eq!(
            b1b_answers(B1C_ORDERS_WITH_FULLTEXT, &output),
            ["unknown", COPY],
            "{form}"
        );
    }
}

#[test]
fn laravel_adding_a_column_to_a_schema_qualified_table_is_unknown_even_without_a_schema_file() {
    // Schema::connection(...)->table('shop.orders', ...): Laravel wraps each
    // part. kilit does not know which database `shop` is.
    let statements = ["alter table `shop`.`orders` add `coupon` varchar(255) null"];
    for (form, output) in [
        ("long", long_form(&[(B1C_NAME, &statements)])),
        ("short", short_form(&[(B1C_NAME, &statements)])),
    ] {
        assert_eq!(b1b_answers(ORDERS, &output), ["unknown"], "{form}");
    }
}

#[test]
fn a_laravel_table_created_in_the_migrations_stays_known_without_a_schema() {
    // Schema::create('tags', ...) then Schema::table('tags', ...): the answer
    // is the lab's, as before.
    let create = "create table `tags` (`id` bigint unsigned not null auto_increment primary key, \
                  `name` varchar(255) not null) default character set utf8mb4 \
                  collate 'utf8mb4_unicode_ci'";
    let statements = [create, "alter table `tags` add `slug` varchar(255) null"];
    for (form, output) in [
        ("long", long_form(&[(B1C_NAME, &statements)])),
        ("short", short_form(&[(B1C_NAME, &statements)])),
    ] {
        assert_eq!(b1b_answers("", &output), [NOT_CHECKED, INSTANT], "{form}");
    }
}

#[test]
fn laravel_s_full_text_index_on_a_table_without_a_schema_makes_writes_wait_in_both_forms() {
    // $table->fullText('note'); the same grammar in Laravel 10 to 13. The lab
    // measured every table it may be (plain, MyISAM, COMPRESSED, COMPACT,
    // REDUNDANT) and each one makes writes wait.
    let statements = ["alter table `orders` add fulltext `orders_note_fulltext`(`note`)"];
    for (form, output) in [
        ("long", long_form(&[(B1C_NAME, &statements)])),
        ("short", short_form(&[(B1C_NAME, &statements)])),
    ] {
        assert_eq!(
            b1b_answers("", &output),
            ["depends on the table: writes wait"],
            "{form}"
        );
    }
}

#[test]
fn laravel_s_full_text_index_keeps_the_lab_answer_on_a_table_the_schema_has() {
    // Regression: add-fulltext-index, INPLACE with a rebuild, writes wait.
    let statements = ["alter table `orders` add fulltext `orders_note_fulltext`(`note`)"];
    for (form, output) in [
        ("long", long_form(&[(B1C_NAME, &statements)])),
        ("short", short_form(&[(B1C_NAME, &statements)])),
    ] {
        assert_eq!(
            b1b_answers(
                "create table `orders` (`id` bigint unsigned not null auto_increment, \
                 `note` varchar(50) default null, primary key (`id`)) \
                 engine=InnoDB default charset=utf8mb4 collate=utf8mb4_unicode_ci;\n",
                &output
            ),
            ["INPLACE, rebuilds the table, writes wait"],
            "{form}"
        );
    }
}
