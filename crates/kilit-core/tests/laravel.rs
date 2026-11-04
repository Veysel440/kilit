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
