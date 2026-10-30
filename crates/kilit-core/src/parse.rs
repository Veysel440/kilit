//! Turns SQL text into kilit's statements, with sqlparser's MySQL dialect.
//!
//! This is the only module that sees sqlparser's types. Anything sqlparser
//! reads but kilit does not model becomes `Other`; anything it cannot read
//! becomes `Unparsed`. Nothing is dropped silently.

use sqlparser::ast::{
    self, AlterTableAlgorithm, AlterTableLock, AlterTableOperation, ColumnOption, ContextModifier,
    Expr, GeneratedExpressionMode, Ident, IndexColumn, MySQLColumnPosition, ObjectName,
    ObjectNamePart, ObjectType, ReferentialAction, RenameTableNameKind, TableConstraint,
};
use sqlparser::dialect::MySqlDialect;
use sqlparser::keywords::Keyword;
use sqlparser::parser::{Parser, ParserError};
use sqlparser::tokenizer::Token;

use crate::split::split;
use crate::statement::{
    Algorithm, Alter, Column, ForeignKey, ForeignKeyAction, Generated, Index, IndexKind, Lock,
    Operation, Position, Rename, Statement, StatementKind, Variable,
};

/// Reads every statement in `sql`, in order.
///
/// A statement that cannot be read becomes [`StatementKind::Unparsed`] and
/// does not stop the others.
pub fn parse(sql: &str) -> Vec<Statement> {
    split(sql)
        .into_iter()
        .map(|piece| Statement {
            line: piece.line,
            sql: piece.sql.to_owned(),
            kind: read(piece.sql),
        })
        .collect()
}

/// Reads one statement.
fn read(sql: &str) -> StatementKind {
    match Parser::parse_sql(&MySqlDialect {}, sql) {
        Ok(statements) => match <[ast::Statement; 1]>::try_from(statements) {
            Ok([statement]) => convert(statement),
            Err(statements) => StatementKind::Unparsed {
                error: format!("expected one statement, found {}", statements.len()),
            },
        },
        Err(err) => StatementKind::Unparsed {
            error: parser_message(err),
        },
    }
}

fn parser_message(err: ParserError) -> String {
    match err {
        ParserError::TokenizerError(message) | ParserError::ParserError(message) => message,
        ParserError::RecursionLimitExceeded => "the statement is nested too deeply".to_owned(),
    }
}

fn convert(statement: ast::Statement) -> StatementKind {
    match statement {
        ast::Statement::AlterTable(alter) => {
            let mut result = Alter {
                table: object_name(&alter.name),
                ..Alter::default()
            };
            for operation in alter.operations {
                apply(&mut result, operation);
            }
            StatementKind::Alter(result)
        }
        // MySQL runs CREATE INDEX as ALTER TABLE ... ADD INDEX.
        ast::Statement::CreateIndex(index) => {
            let mut result = Alter {
                table: object_name(&index.table_name),
                ..Alter::default()
            };
            result.operations.push(Operation::AddIndex(Index {
                name: index.name.as_ref().map(object_name),
                kind: if index.unique {
                    IndexKind::Unique
                } else {
                    IndexKind::Plain
                },
                columns: index.columns.iter().map(index_column).collect(),
            }));
            // `ALGORITHM=...` and `LOCK=...` after the column list.
            for option in index.alter_options {
                apply(&mut result, option);
            }
            StatementKind::Alter(result)
        }
        // And DROP INDEX ... ON ... as ALTER TABLE ... DROP INDEX.
        ast::Statement::Drop {
            object_type: ObjectType::Index,
            names,
            table: Some(table),
            ..
        } => StatementKind::Alter(Alter {
            table: object_name(&table),
            operations: names
                .iter()
                .map(|name| Operation::DropIndex {
                    name: object_name(name),
                })
                .collect(),
            ..Alter::default()
        }),
        ast::Statement::Drop {
            object_type: ObjectType::Table,
            names,
            ..
        } => StatementKind::DropTable {
            tables: names.iter().map(object_name).collect(),
        },
        ast::Statement::CreateTable(table) => StatementKind::CreateTable {
            table: object_name(&table.name),
        },
        ast::Statement::RenameTable(renames) => StatementKind::RenameTable {
            renames: renames
                .iter()
                .map(|rename| Rename {
                    from: object_name(&rename.old_name),
                    to: object_name(&rename.new_name),
                })
                .collect(),
        },
        ast::Statement::Set(set) => set_statement(set),
        _ => StatementKind::Other,
    }
}

/// Adds one parsed `ALTER TABLE` operation to `alter`.
fn apply(alter: &mut Alter, operation: AlterTableOperation) {
    match operation {
        AlterTableOperation::Algorithm { algorithm, .. } => {
            alter.algorithm = forced_algorithm(algorithm);
        }
        AlterTableOperation::Lock { lock, .. } => alter.lock = requested_lock(lock),
        // `DROP COLUMN a, b` is one operation for the parser, two for kilit.
        AlterTableOperation::DropColumn { column_names, .. } => {
            for name in column_names {
                alter
                    .operations
                    .push(Operation::DropColumn { name: name.value });
            }
        }
        other => alter.operations.push(operation_of(other)),
    }
}

/// Converts the operations that map to exactly one of kilit's operations.
fn operation_of(operation: AlterTableOperation) -> Operation {
    match operation {
        AlterTableOperation::AddColumn {
            column_def,
            column_position,
            ..
        } => Operation::AddColumn {
            column: column(
                column_def.name,
                &column_def.data_type,
                column_def.options.into_iter().map(|option| option.option),
            ),
            position: column_position.map(position),
        },
        AlterTableOperation::ModifyColumn {
            col_name,
            data_type,
            options,
            column_position,
        } => Operation::ModifyColumn {
            column: column(col_name, &data_type, options),
            position: column_position.map(position),
        },
        AlterTableOperation::ChangeColumn {
            old_name,
            new_name,
            data_type,
            options,
            column_position,
        } => Operation::ChangeColumn {
            old_name: old_name.value,
            column: column(new_name, &data_type, options),
            position: column_position.map(position),
        },
        AlterTableOperation::RenameColumn {
            old_column_name,
            new_column_name,
        } => Operation::RenameColumn {
            from: old_column_name.value,
            to: new_column_name.value,
        },
        AlterTableOperation::AddConstraint { constraint, .. } => added_constraint(constraint),
        AlterTableOperation::DropIndex { name } => Operation::DropIndex { name: name.value },
        AlterTableOperation::DropForeignKey { name, .. } => {
            Operation::DropForeignKey { name: name.value }
        }
        AlterTableOperation::DropPrimaryKey { .. } => Operation::DropPrimaryKey,
        AlterTableOperation::RenameTable { table_name } => {
            let (RenameTableNameKind::As(name) | RenameTableNameKind::To(name)) = table_name;
            Operation::RenameTable {
                new_name: object_name(&name),
            }
        }
        other => Operation::Other {
            sql: other.to_string(),
        },
    }
}

/// `ADD INDEX`, `ADD UNIQUE`, `ADD PRIMARY KEY`, `ADD FOREIGN KEY` and friends.
fn added_constraint(constraint: TableConstraint) -> Operation {
    match constraint {
        TableConstraint::Index(index) => Operation::AddIndex(Index {
            name: index.name.map(|name| name.value),
            kind: IndexKind::Plain,
            columns: index.columns.iter().map(index_column).collect(),
        }),
        // `ADD CONSTRAINT symbol UNIQUE INDEX name (...)`: MySQL names the
        // index `name` if given, else `symbol`.
        TableConstraint::Unique(unique) => Operation::AddIndex(Index {
            name: unique.index_name.or(unique.name).map(|name| name.value),
            kind: IndexKind::Unique,
            columns: unique.columns.iter().map(index_column).collect(),
        }),
        TableConstraint::FulltextOrSpatial(index) => Operation::AddIndex(Index {
            name: index.opt_index_name.map(|name| name.value),
            kind: if index.fulltext {
                IndexKind::Fulltext
            } else {
                IndexKind::Spatial
            },
            columns: index.columns.iter().map(index_column).collect(),
        }),
        TableConstraint::PrimaryKey(key) => Operation::AddPrimaryKey {
            columns: key.columns.iter().map(index_column).collect(),
        },
        TableConstraint::ForeignKey(key) => Operation::AddForeignKey(ForeignKey {
            name: key.name.map(|name| name.value),
            columns: key.columns.into_iter().map(|column| column.value).collect(),
            referenced_table: object_name(&key.foreign_table),
            referenced_columns: key
                .referred_columns
                .into_iter()
                .map(|column| column.value)
                .collect(),
            on_delete: key.on_delete.map(foreign_key_action),
            on_update: key.on_update.map(foreign_key_action),
        }),
        other => Operation::Other {
            sql: format!("ADD {other}"),
        },
    }
}

fn column(
    name: Ident,
    data_type: &ast::DataType,
    options: impl IntoIterator<Item = ColumnOption>,
) -> Column {
    let mut column = Column {
        name: name.value,
        data_type: data_type.to_string(),
        ..Column::default()
    };
    for option in options {
        match option {
            ColumnOption::Null => column.nullable = Some(true),
            ColumnOption::NotNull => column.nullable = Some(false),
            ColumnOption::Default(value) => column.default = Some(value.to_string()),
            ColumnOption::DialectSpecific(tokens) if is_auto_increment(&tokens) => {
                column.auto_increment = true;
            }
            ColumnOption::CharacterSet(name) => column.charset = Some(object_name(&name)),
            ColumnOption::Collation(name) => column.collation = Some(object_name(&name)),
            ColumnOption::Comment(text) => column.comment = Some(text),
            ColumnOption::OnUpdate(value) => column.on_update = Some(value.to_string()),
            ColumnOption::Generated {
                generation_expr: Some(expression),
                generation_expr_mode,
                ..
            } => {
                column.generated = Some(Generated {
                    expression: expression.to_string(),
                    stored: generation_expr_mode == Some(GeneratedExpressionMode::Stored),
                });
            }
            other => column.other_options.push(other.to_string()),
        }
    }
    column
}

/// MySQL's `AUTO_INCREMENT` reaches us as a dialect-specific keyword.
fn is_auto_increment(tokens: &[Token]) -> bool {
    matches!(tokens, [Token::Word(word)] if word.keyword == Keyword::AUTO_INCREMENT)
}

fn position(position: MySQLColumnPosition) -> Position {
    match position {
        MySQLColumnPosition::First => Position::First,
        MySQLColumnPosition::After(column) => Position::After(column.value),
    }
}

/// A plain column keeps its name without quotes; prefixes and expressions
/// (`email(10)`, `(lower(email))`) are kept as the parser prints them.
fn index_column(column: &IndexColumn) -> String {
    match &column.column.expr {
        Expr::Identifier(ident) => ident.value.clone(),
        other => other.to_string(),
    }
}

/// `` `shop`.`orders` `` becomes `shop.orders`.
fn object_name(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|part| match part {
            ObjectNamePart::Identifier(ident) => ident.value.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// `ALGORITHM=DEFAULT` forces nothing.
fn forced_algorithm(algorithm: AlterTableAlgorithm) -> Option<Algorithm> {
    match algorithm {
        AlterTableAlgorithm::Default => None,
        AlterTableAlgorithm::Instant => Some(Algorithm::Instant),
        AlterTableAlgorithm::Inplace => Some(Algorithm::Inplace),
        AlterTableAlgorithm::Copy => Some(Algorithm::Copy),
    }
}

/// `LOCK=DEFAULT` asks for nothing.
fn requested_lock(lock: AlterTableLock) -> Option<Lock> {
    match lock {
        AlterTableLock::Default => None,
        AlterTableLock::None => Some(Lock::None),
        AlterTableLock::Shared => Some(Lock::Shared),
        AlterTableLock::Exclusive => Some(Lock::Exclusive),
    }
}

fn foreign_key_action(action: ReferentialAction) -> ForeignKeyAction {
    match action {
        ReferentialAction::Restrict => ForeignKeyAction::Restrict,
        ReferentialAction::Cascade => ForeignKeyAction::Cascade,
        ReferentialAction::SetNull => ForeignKeyAction::SetNull,
        ReferentialAction::NoAction => ForeignKeyAction::NoAction,
        ReferentialAction::SetDefault => ForeignKeyAction::SetDefault,
    }
}

/// Session variables become `Set`. A statement that also sets global or
/// user variables becomes `Other`: kilit does not follow those.
fn set_statement(set: ast::Set) -> StatementKind {
    let assignments: Vec<(Option<ContextModifier>, ObjectName, Expr)> = match set {
        ast::Set::SingleAssignment {
            scope,
            hivevar: false,
            variable,
            mut values,
        } if values.len() == 1 => vec![(scope, variable, values.remove(0))],
        ast::Set::MultipleAssignments { assignments } => assignments
            .into_iter()
            .map(|assignment| (assignment.scope, assignment.name, assignment.value))
            .collect(),
        _ => return StatementKind::Other,
    };

    let mut variables = Vec::with_capacity(assignments.len());
    for (scope, name, value) in assignments {
        let Some(name) = session_variable(scope, &name) else {
            return StatementKind::Other;
        };
        variables.push(Variable {
            name,
            value: value.to_string(),
        });
    }
    StatementKind::Set { variables }
}

/// The lowercase name of a session variable, or `None` for a global or a
/// user variable. `SESSION x`, `LOCAL x`, `@@session.x`, `@@x` and plain
/// `x` all mean the session's `x`.
fn session_variable(scope: Option<ContextModifier>, name: &ObjectName) -> Option<String> {
    if scope == Some(ContextModifier::Global) {
        return None;
    }
    let name = object_name(name).to_lowercase();
    if name.starts_with("@@global.") {
        return None;
    }
    let name = ["@@session.", "@@local.", "@@"]
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))
        .unwrap_or(&name);
    if name.starts_with('@') {
        // `@x` is a user variable: it changes nothing about how DDL runs.
        return None;
    }
    Some(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kind of the only statement in `sql`.
    fn kind(sql: &str) -> StatementKind {
        let mut statements = parse(sql);
        assert_eq!(statements.len(), 1, "{sql}");
        statements.remove(0).kind
    }

    fn alter(sql: &str) -> Alter {
        match kind(sql) {
            StatementKind::Alter(alter) => alter,
            other => panic!("{sql}: expected an ALTER TABLE, got {other:?}"),
        }
    }

    /// The only operation of an ALTER TABLE.
    fn operation(sql: &str) -> Operation {
        let mut alter = alter(sql);
        assert_eq!(alter.operations.len(), 1, "{sql}");
        alter.operations.remove(0)
    }

    fn added_column(sql: &str) -> Column {
        match operation(sql) {
            Operation::AddColumn { column, .. } => column,
            other => panic!("{sql}: expected ADD COLUMN, got {other:?}"),
        }
    }

    #[test]
    fn reads_the_table_name_without_quotes() {
        assert_eq!(alter("ALTER TABLE `orders` DROP note").table, "orders");
        assert_eq!(
            alter("ALTER TABLE shop.orders DROP note").table,
            "shop.orders"
        );
    }

    #[test]
    fn reads_algorithm_and_lock_clauses() {
        let forced = alter("ALTER TABLE t ADD c INT, ALGORITHM=INPLACE, LOCK=NONE");
        assert_eq!(forced.algorithm, Some(Algorithm::Inplace));
        assert_eq!(forced.lock, Some(Lock::None));
        assert_eq!(forced.operations.len(), 1);

        let defaults = alter("ALTER TABLE t ADD c INT, ALGORITHM=DEFAULT, LOCK=DEFAULT");
        assert_eq!(defaults.algorithm, None);
        assert_eq!(defaults.lock, None);
    }

    #[test]
    fn reads_column_options() {
        let column = added_column(
            "ALTER TABLE t ADD c VARCHAR(50) CHARACTER SET utf8mb4 COLLATE 'utf8mb4_bin' \
             NOT NULL DEFAULT 'x' COMMENT 'hello'",
        );
        assert_eq!(
            column,
            Column {
                name: "c".into(),
                data_type: "VARCHAR(50)".into(),
                nullable: Some(false),
                default: Some("'x'".into()),
                charset: Some("utf8mb4".into()),
                collation: Some("utf8mb4_bin".into()),
                comment: Some("hello".into()),
                ..Column::default()
            }
        );
    }

    #[test]
    fn nullability_is_only_known_when_written() {
        assert_eq!(added_column("ALTER TABLE t ADD c INT").nullable, None);
        assert_eq!(
            added_column("ALTER TABLE t ADD c INT NULL").nullable,
            Some(true)
        );
    }

    #[test]
    fn reads_auto_increment_and_on_update() {
        let id = added_column("ALTER TABLE t ADD id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT");
        assert!(id.auto_increment);
        assert_eq!(id.data_type, "BIGINT UNSIGNED");

        let updated = added_column(
            "ALTER TABLE t ADD updated_at TIMESTAMP NULL DEFAULT CURRENT_TIMESTAMP \
             ON UPDATE CURRENT_TIMESTAMP",
        );
        assert_eq!(updated.default.as_deref(), Some("CURRENT_TIMESTAMP"));
        assert_eq!(updated.on_update.as_deref(), Some("CURRENT_TIMESTAMP"));
    }

    #[test]
    fn reads_generated_columns() {
        let stored = added_column("ALTER TABLE t ADD g INT AS (amount + 1) STORED");
        assert_eq!(
            stored.generated,
            Some(Generated {
                expression: "amount + 1".into(),
                stored: true,
            })
        );
        // VIRTUAL is MySQL's default.
        let virtual_column = added_column("ALTER TABLE t ADD g INT AS (amount + 1)");
        assert!(!virtual_column.generated.unwrap().stored);
    }

    #[test]
    fn unknown_column_options_are_kept() {
        let column = added_column("ALTER TABLE t ADD c INT NOT NULL UNIQUE");
        assert_eq!(column.nullable, Some(false));
        assert_eq!(column.other_options, ["UNIQUE"]);
    }

    #[test]
    fn reads_column_positions() {
        let first = operation("ALTER TABLE t ADD c INT FIRST");
        assert!(matches!(
            first,
            Operation::AddColumn {
                position: Some(Position::First),
                ..
            }
        ));
        let after = operation("ALTER TABLE t MODIFY c INT AFTER b");
        assert!(matches!(
            after,
            Operation::ModifyColumn { position: Some(Position::After(ref b)), .. } if b == "b"
        ));
    }

    #[test]
    fn change_column_keeps_both_names() {
        let Operation::ChangeColumn {
            old_name, column, ..
        } = operation("ALTER TABLE t CHANGE note memo VARCHAR(50) NULL")
        else {
            panic!("expected CHANGE");
        };
        assert_eq!(old_name, "note");
        assert_eq!(column.name, "memo");
    }

    #[test]
    fn each_dropped_column_is_one_operation() {
        assert_eq!(
            alter("ALTER TABLE t DROP a, DROP COLUMN b").operations,
            [
                Operation::DropColumn { name: "a".into() },
                Operation::DropColumn { name: "b".into() },
            ]
        );
    }

    #[test]
    fn reads_index_kinds() {
        let kinds = [
            ("ADD INDEX i (a)", IndexKind::Plain),
            ("ADD KEY i (a)", IndexKind::Plain),
            ("ADD UNIQUE i (a)", IndexKind::Unique),
            ("ADD CONSTRAINT i UNIQUE (a)", IndexKind::Unique),
            ("ADD FULLTEXT i (a)", IndexKind::Fulltext),
            ("ADD SPATIAL INDEX i (a)", IndexKind::Spatial),
        ];
        for (sql, kind) in kinds {
            let sql = format!("ALTER TABLE t {sql}");
            assert_eq!(
                operation(&sql),
                Operation::AddIndex(Index {
                    name: Some("i".into()),
                    kind,
                    columns: vec!["a".into()],
                }),
                "{sql}"
            );
        }
    }

    #[test]
    fn a_unique_index_takes_its_own_name_over_the_constraint_name() {
        // Checked on MySQL 8.0.46 and 8.4.13: this index is named `i`.
        let Operation::AddIndex(index) =
            operation("ALTER TABLE t ADD CONSTRAINT c UNIQUE INDEX i (a)")
        else {
            panic!("expected ADD INDEX");
        };
        assert_eq!(index.name.as_deref(), Some("i"));
    }

    #[test]
    fn index_prefixes_are_kept() {
        let Operation::AddIndex(index) = operation("ALTER TABLE t ADD INDEX (email(10), id)")
        else {
            panic!("expected ADD INDEX");
        };
        assert_eq!(index.name, None);
        assert_eq!(index.columns, ["email(10)", "id"]);
    }

    #[test]
    fn reads_primary_keys() {
        assert_eq!(
            alter("ALTER TABLE t DROP PRIMARY KEY, ADD PRIMARY KEY (id, user_id)").operations,
            [
                Operation::DropPrimaryKey,
                Operation::AddPrimaryKey {
                    columns: vec!["id".into(), "user_id".into()],
                },
            ]
        );
    }

    #[test]
    fn reads_foreign_keys() {
        let operation = operation(
            "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id) \
             ON DELETE SET NULL ON UPDATE CASCADE",
        );
        assert_eq!(
            operation,
            Operation::AddForeignKey(ForeignKey {
                name: Some("fk".into()),
                columns: vec!["user_id".into()],
                referenced_table: "users".into(),
                referenced_columns: vec!["id".into()],
                on_delete: Some(ForeignKeyAction::SetNull),
                on_update: Some(ForeignKeyAction::Cascade),
            })
        );
    }

    #[test]
    fn create_and_drop_index_are_alters() {
        let created = alter("CREATE UNIQUE INDEX i ON t (a) ALGORITHM=INPLACE LOCK=NONE");
        assert_eq!(created.table, "t");
        assert_eq!(created.algorithm, Some(Algorithm::Inplace));
        assert_eq!(created.lock, Some(Lock::None));
        assert!(matches!(
            created.operations[..],
            [Operation::AddIndex(Index {
                kind: IndexKind::Unique,
                ..
            })]
        ));

        let dropped = alter("DROP INDEX i ON t");
        assert_eq!(
            dropped.operations,
            [Operation::DropIndex { name: "i".into() }]
        );
    }

    #[test]
    fn reads_table_statements() {
        assert_eq!(
            kind("CREATE TABLE shops (id BIGINT)"),
            StatementKind::CreateTable {
                table: "shops".into()
            }
        );
        assert_eq!(
            kind("DROP TABLE IF EXISTS a, b"),
            StatementKind::DropTable {
                tables: vec!["a".into(), "b".into()]
            }
        );
        assert_eq!(
            kind("RENAME TABLE a TO b, c TO d"),
            StatementKind::RenameTable {
                renames: vec![
                    Rename {
                        from: "a".into(),
                        to: "b".into()
                    },
                    Rename {
                        from: "c".into(),
                        to: "d".into()
                    },
                ]
            }
        );
        assert_eq!(
            operation("ALTER TABLE a RENAME TO b"),
            Operation::RenameTable {
                new_name: "b".into()
            }
        );
    }

    #[test]
    fn reads_session_variables() {
        let foreign_key_checks_off = StatementKind::Set {
            variables: vec![Variable {
                name: "foreign_key_checks".into(),
                value: "0".into(),
            }],
        };
        for sql in [
            "SET FOREIGN_KEY_CHECKS=0",
            "SET SESSION foreign_key_checks = 0",
            "SET LOCAL foreign_key_checks = 0",
            "SET @@session.foreign_key_checks = 0",
            "SET @@foreign_key_checks = 0",
        ] {
            assert_eq!(kind(sql), foreign_key_checks_off, "{sql}");
        }
        assert_eq!(
            kind("SET foreign_key_checks = OFF, unique_checks = 0"),
            StatementKind::Set {
                variables: vec![
                    Variable {
                        name: "foreign_key_checks".into(),
                        value: "OFF".into(),
                    },
                    Variable {
                        name: "unique_checks".into(),
                        value: "0".into(),
                    },
                ]
            }
        );
    }

    #[test]
    fn global_and_user_variables_are_not_followed() {
        for sql in [
            "SET GLOBAL foreign_key_checks = 0",
            "SET @@global.foreign_key_checks = 0",
            "SET @old = @@foreign_key_checks",
            "SET NAMES utf8mb4",
        ] {
            assert_eq!(kind(sql), StatementKind::Other, "{sql}");
        }
    }

    #[test]
    fn statements_kilit_does_not_analyze_are_other() {
        assert_eq!(kind("INSERT INTO t VALUES (1)"), StatementKind::Other);
        assert_eq!(kind("UPDATE t SET a = 1"), StatementKind::Other);
    }

    #[test]
    fn operations_kilit_does_not_model_are_kept_as_other() {
        assert_eq!(
            operation("ALTER TABLE t AUTO_INCREMENT = 1000"),
            Operation::Other {
                sql: "AUTO_INCREMENT = 1000".into()
            }
        );
        assert!(matches!(
            operation("ALTER TABLE t ADD CHECK (a > 0)"),
            Operation::Other { ref sql } if sql.starts_with("ADD CHECK")
        ));
    }

    #[test]
    fn statements_sqlparser_cannot_read_are_unparsed() {
        // All valid MySQL that sqlparser 0.63 does not read yet.
        for sql in [
            "ALTER TABLE t CONVERT TO CHARACTER SET utf8mb4",
            "ALTER TABLE t ENGINE=InnoDB",
            "ALTER TABLE t RENAME INDEX a TO b",
            "ALTER TABLE t ADD c INT, ALGORITHM=NOCOPY",
            "OPTIMIZE TABLE t",
        ] {
            assert!(
                matches!(kind(sql), StatementKind::Unparsed { ref error } if !error.is_empty()),
                "{sql}"
            );
        }
    }

    #[test]
    fn deep_nesting_is_unparsed_not_a_crash() {
        let nested = format!(
            "ALTER TABLE t ADD c INT DEFAULT {}1{}",
            "(".repeat(500),
            ")".repeat(500)
        );
        assert_eq!(
            kind(&nested),
            StatementKind::Unparsed {
                error: "the statement is nested too deeply".into()
            }
        );
    }

    #[test]
    fn one_bad_statement_does_not_hide_the_others() {
        let sql = "\
-- a migration
SET FOREIGN_KEY_CHECKS=0;
ALTER TABLE orders ADD coupon VARCHAR(255) NULL;
ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4;
INSERT INTO settings VALUES ('separator', ';');
SET FOREIGN_KEY_CHECKS=1;
";
        let statements = parse(sql);
        let summary: Vec<(usize, &str)> = statements
            .iter()
            .map(|statement| {
                let kind = match statement.kind {
                    StatementKind::Alter(_) => "alter",
                    StatementKind::Set { .. } => "set",
                    StatementKind::Other => "other",
                    StatementKind::Unparsed { .. } => "unparsed",
                    _ => "?",
                };
                (statement.line, kind)
            })
            .collect();
        assert_eq!(
            summary,
            [
                (2, "set"),
                (3, "alter"),
                (4, "unparsed"),
                (5, "other"),
                (6, "set")
            ]
        );
        assert_eq!(
            statements[3].sql,
            "INSERT INTO settings VALUES ('separator', ';')"
        );
    }
}
