//! Turns SQL text into kilit's statements, with sqlparser's MySQL dialect.
//!
//! This is the only module that sees sqlparser's types. Anything sqlparser
//! reads but kilit does not model becomes `Other`; anything it cannot read
//! becomes `Unparsed`. Nothing is dropped silently.

use sqlparser::ast::{
    self, AlterColumnOperation, AlterTableAlgorithm, AlterTableLock, AlterTableOperation,
    CharacterLength, ColumnOption, ContextModifier, CreateTableOptions, Expr, ForeignKeyConstraint,
    GeneratedExpressionMode, Ident, IndexColumn, MySQLColumnPosition, ObjectName, ObjectNamePart,
    ObjectType, ReferentialAction, RenameTableNameKind, SqlOption, TableConstraint, Value,
};
use sqlparser::dialect::MySqlDialect;
use sqlparser::keywords::Keyword;
use sqlparser::parser::{Parser, ParserError};
use sqlparser::tokenizer::{Token, Tokenizer};

use crate::split::split;
use crate::statement::{
    Algorithm, Alter, Column, DataType, ForeignKey, ForeignKeyAction, Generated, Index, IndexKind,
    IntegerSize, Lock, Operation, Position, Rename, Statement, StatementKind, Table, Variable,
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
                table: table_named_in(sql),
            },
        },
        Err(err) => StatementKind::Unparsed {
            error: parser_message(err),
            table: table_named_in(sql),
        },
    }
}

/// The table a statement names after the word `TABLE`, such as `orders` in
/// `ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4`, found from the
/// words alone for a statement the parser cannot read.
fn table_named_in(sql: &str) -> Option<String> {
    let tokens = Tokenizer::new(&MySqlDialect {}, sql).tokenize().ok()?;
    let mut words = tokens
        .into_iter()
        .filter(|token| !matches!(token, Token::Whitespace(_)));
    words.find(|token| matches!(token, Token::Word(word) if word.keyword == Keyword::TABLE))?;
    let Token::Word(first) = words.next()? else {
        return None;
    };
    // `shop.orders`
    if let (Some(Token::Period), Some(Token::Word(table))) = (words.next(), words.next()) {
        return Some(format!("{}.{}", first.value, table.value));
    }
    Some(first.value)
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
        ast::Statement::CreateTable(create) => create_table(create),
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

/// `CREATE TABLE` with its columns, keys and default character set.
fn create_table(create: ast::CreateTable) -> StatementKind {
    let name = object_name(&create.name);
    if create.like.is_some() || create.query.is_some() || create.clone.is_some() {
        return StatementKind::Unparsed {
            error: "the table takes its columns from elsewhere (LIKE or AS SELECT), \
                    which kilit does not follow yet"
                .to_owned(),
            table: Some(name),
        };
    }
    let mut table = Table {
        name,
        ..Table::default()
    };
    for column_def in create.columns {
        let column_name = column_def.name.value.clone();
        let mut options = Vec::new();
        for option in column_def.options {
            match option.option {
                // Keys written on a column belong to the table, as MySQL
                // keeps them. An inline unique index is named after its column.
                ColumnOption::PrimaryKey(_) => table.primary_key = vec![column_name.clone()],
                ColumnOption::Unique(_) => table.indexes.push(Index {
                    name: Some(column_name.clone()),
                    kind: IndexKind::Unique,
                    columns: vec![column_name.clone()],
                }),
                // MySQL ignores an inline REFERENCES, and keeps a CHECK as a
                // table constraint that a later MODIFY leaves alone.
                ColumnOption::ForeignKey(_) | ColumnOption::Check(_) => {}
                other => options.push(other),
            }
        }
        table
            .columns
            .push(column(column_def.name, &column_def.data_type, options));
    }
    for constraint in create.constraints {
        match constraint {
            TableConstraint::PrimaryKey(key) => {
                table.primary_key = key.columns.iter().map(index_column).collect();
            }
            TableConstraint::ForeignKey(key) => table.foreign_keys.push(foreign_key(key)),
            other => {
                if let Operation::AddIndex(index) = added_constraint(other) {
                    table.indexes.push(index);
                }
            }
        }
    }
    (table.charset, table.collation) = table_charset(&create.table_options);
    StatementKind::CreateTable {
        table,
        if_not_exists: create.if_not_exists,
    }
}

/// The table's `DEFAULT CHARSET` and `COLLATE`, in any of their spellings:
/// mysqldump writes `DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci`,
/// Laravel `default character set utf8mb4 collate 'utf8mb4_unicode_ci'`.
fn table_charset(options: &CreateTableOptions) -> (Option<String>, Option<String>) {
    let (CreateTableOptions::Plain(options)
    | CreateTableOptions::With(options)
    | CreateTableOptions::Options(options)
    | CreateTableOptions::TableProperties(options)) = options
    else {
        return (None, None);
    };
    let (mut charset, mut collation) = (None, None);
    for option in options {
        let SqlOption::KeyValue { key, value } = option else {
            continue;
        };
        let key = key.value.to_ascii_uppercase();
        match key.strip_prefix("DEFAULT ").unwrap_or(&key) {
            "CHARSET" | "CHARACTER SET" => charset = Some(option_value(value)),
            "COLLATE" => collation = Some(option_value(value)),
            _ => {}
        }
    }
    (charset, collation)
}

/// `utf8mb4`, whether written as a name or as a string.
fn option_value(value: &Expr) -> String {
    match value {
        Expr::Identifier(ident) => ident.value.clone(),
        Expr::Value(value) => match &value.value {
            Value::SingleQuotedString(text) | Value::DoubleQuotedString(text) => text.clone(),
            other => other.to_string(),
        },
        other => other.to_string(),
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
        AlterTableOperation::AlterColumn {
            column_name,
            op: AlterColumnOperation::SetDefault { value },
        } => Operation::SetColumnDefault {
            column: column_name.value,
            default: value.to_string(),
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
        TableConstraint::ForeignKey(key) => Operation::AddForeignKey(foreign_key(key)),
        other => Operation::Other {
            sql: format!("ADD {other}"),
        },
    }
}

fn foreign_key(key: ForeignKeyConstraint) -> ForeignKey {
    ForeignKey {
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
    }
}

fn column(
    name: Ident,
    data_type: &ast::DataType,
    options: impl IntoIterator<Item = ColumnOption>,
) -> Column {
    let mut column = Column {
        name: name.value,
        data_type: kilit_type(data_type),
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

/// Integers and VARCHAR in detail, other text types and the rest as the
/// parser prints them.
fn kilit_type(data_type: &ast::DataType) -> DataType {
    use ast::DataType as Parsed;
    let integer = |size, unsigned| DataType::Integer { size, unsigned };
    match data_type {
        // MySQL's BOOLEAN is TINYINT(1).
        Parsed::TinyInt(_) | Parsed::Bool | Parsed::Boolean => integer(IntegerSize::Tiny, false),
        Parsed::TinyIntUnsigned(_) => integer(IntegerSize::Tiny, true),
        Parsed::SmallInt(_) => integer(IntegerSize::Small, false),
        Parsed::SmallIntUnsigned(_) => integer(IntegerSize::Small, true),
        Parsed::MediumInt(_) => integer(IntegerSize::Medium, false),
        Parsed::MediumIntUnsigned(_) => integer(IntegerSize::Medium, true),
        Parsed::Int(_) | Parsed::Integer(_) => integer(IntegerSize::Int, false),
        Parsed::IntUnsigned(_) | Parsed::IntegerUnsigned(_) => integer(IntegerSize::Int, true),
        Parsed::BigInt(_) => integer(IntegerSize::Big, false),
        Parsed::BigIntUnsigned(_) => integer(IntegerSize::Big, true),
        Parsed::Varchar(Some(CharacterLength::IntegerLength { length, .. }))
        | Parsed::CharacterVarying(Some(CharacterLength::IntegerLength { length, .. }))
        | Parsed::CharVarying(Some(CharacterLength::IntegerLength { length, .. })) => {
            DataType::VarChar { length: *length }
        }
        Parsed::Char(_)
        | Parsed::Character(_)
        | Parsed::Text
        | Parsed::TinyText
        | Parsed::MediumText
        | Parsed::LongText
        | Parsed::Enum(..)
        | Parsed::Set(_) => DataType::Text(data_type.to_string()),
        other => DataType::Other(other.to_string()),
    }
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

/// Session variables become `Set`. Global and user variables in the same
/// statement are skipped: they do not change how this session runs DDL.
/// A statement with no session variable at all becomes `Other`.
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

    let variables: Vec<Variable> = assignments
        .into_iter()
        .filter_map(|(scope, name, value)| {
            Some(Variable {
                name: session_variable(scope, &name)?,
                value: value.to_string(),
            })
        })
        .collect();
    if variables.is_empty() {
        StatementKind::Other
    } else {
        StatementKind::Set { variables }
    }
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
                data_type: DataType::VarChar { length: 50 },
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
        assert_eq!(id.data_type.to_string(), "BIGINT UNSIGNED");

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
    fn reads_set_default() {
        assert_eq!(
            operation("ALTER TABLE t ALTER COLUMN amount SET DEFAULT 0"),
            Operation::SetColumnDefault {
                column: "amount".into(),
                default: "0".into(),
            }
        );
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
        assert!(matches!(
            kind("CREATE TABLE shops (id BIGINT)"),
            StatementKind::CreateTable { ref table, if_not_exists: false } if table.name == "shops"
        ));
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
    fn session_variables_are_kept_next_to_user_variables() {
        // How mysqldump turns the checks off while saving the old value.
        assert_eq!(
            kind("SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0"),
            StatementKind::Set {
                variables: vec![Variable {
                    name: "foreign_key_checks".into(),
                    value: "0".into(),
                }]
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
                matches!(
                    kind(sql),
                    StatementKind::Unparsed { ref error, ref table }
                        if !error.is_empty() && table.as_deref() == Some("t")
                ),
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
                error: "the statement is nested too deeply".into(),
                table: Some("t".into()),
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

    #[test]
    fn reads_data_types() {
        for (written, read) in [
            ("INT", "INT"),
            ("INTEGER", "INT"),
            ("int(11)", "INT"),
            ("INT(10) UNSIGNED", "INT UNSIGNED"),
            ("bigint unsigned", "BIGINT UNSIGNED"),
            ("tinyint(1)", "TINYINT"),
            ("BOOLEAN", "TINYINT"),
            ("varchar(50)", "VARCHAR(50)"),
            ("CHARACTER VARYING(20)", "VARCHAR(20)"),
        ] {
            let column = added_column(&format!("ALTER TABLE t ADD c {written}"));
            assert_eq!(column.data_type.to_string(), read, "{written}");
        }
        for (written, text) in [
            ("char(36)", true),
            ("TEXT", true),
            ("enum('a','b')", true),
            ("decimal(8,2)", false),
            ("json", false),
            ("varbinary(16)", false),
        ] {
            let column = added_column(&format!("ALTER TABLE t ADD c {written}"));
            assert_eq!(column.data_type.is_text(), text, "{written}");
        }
    }

    fn created(sql: &str) -> Table {
        match kind(sql) {
            StatementKind::CreateTable { table, .. } => table,
            other => panic!("{sql}: expected CREATE TABLE, got {other:?}"),
        }
    }

    #[test]
    fn reads_a_table_from_a_schema_dump() {
        // As mysqldump writes it, and so `php artisan schema:dump`.
        let table = created(
            "CREATE TABLE `orders` (
  `id` bigint unsigned NOT NULL AUTO_INCREMENT,
  `user_id` bigint unsigned NOT NULL,
  `status` varchar(20) COLLATE utf8mb4_unicode_ci NOT NULL DEFAULT 'new',
  `note` varchar(50) COLLATE utf8mb4_unicode_ci DEFAULT NULL,
  PRIMARY KEY (`id`),
  KEY `orders_user_id_foreign` (`user_id`),
  FULLTEXT KEY `orders_note_fulltext` (`note`),
  CONSTRAINT `orders_user_id_foreign` FOREIGN KEY (`user_id`) REFERENCES `users` (`id`) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci",
        );
        assert_eq!(table.name, "orders");
        let names: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["id", "user_id", "status", "note"]);
        assert_eq!(
            table.columns[2],
            Column {
                name: "status".into(),
                data_type: DataType::VarChar { length: 20 },
                nullable: Some(false),
                default: Some("'new'".into()),
                collation: Some("utf8mb4_unicode_ci".into()),
                ..Column::default()
            }
        );
        assert_eq!(table.columns[3].default.as_deref(), Some("NULL"));
        assert_eq!(table.primary_key, ["id"]);
        assert_eq!(
            table.indexes,
            [
                Index {
                    name: Some("orders_user_id_foreign".into()),
                    kind: IndexKind::Plain,
                    columns: vec!["user_id".into()],
                },
                Index {
                    name: Some("orders_note_fulltext".into()),
                    kind: IndexKind::Fulltext,
                    columns: vec!["note".into()],
                },
            ]
        );
        assert_eq!(table.foreign_keys.len(), 1);
        assert_eq!(table.foreign_keys[0].referenced_table, "users");
        assert_eq!(table.charset.as_deref(), Some("utf8mb4"));
        assert_eq!(table.collation.as_deref(), Some("utf8mb4_unicode_ci"));
    }

    #[test]
    fn keys_written_on_a_column_belong_to_the_table() {
        let table = created(
            "CREATE TABLE IF NOT EXISTS shops (id BIGINT UNSIGNED AUTO_INCREMENT PRIMARY KEY, \
             code VARCHAR(10) NOT NULL UNIQUE) DEFAULT CHARACTER SET latin1",
        );
        assert_eq!(table.primary_key, ["id"]);
        assert_eq!(
            table.indexes,
            [Index {
                name: Some("code".into()),
                kind: IndexKind::Unique,
                columns: vec!["code".into()],
            }]
        );
        assert!(table.columns.iter().all(|c| c.other_options.is_empty()));
        assert_eq!(table.charset.as_deref(), Some("latin1"));
        assert!(matches!(
            kind("CREATE TABLE IF NOT EXISTS shops (id INT)"),
            StatementKind::CreateTable {
                if_not_exists: true,
                ..
            }
        ));
    }

    #[test]
    fn a_table_copied_from_elsewhere_is_unparsed() {
        for sql in [
            "CREATE TABLE new_orders LIKE orders",
            "CREATE TABLE new_orders AS SELECT * FROM orders",
        ] {
            assert!(
                matches!(
                    kind(sql),
                    StatementKind::Unparsed { table: Some(ref table), .. } if table == "new_orders"
                ),
                "{sql}"
            );
        }
    }

    #[test]
    fn unreadable_statements_name_their_table() {
        for (sql, table) in [
            (
                "ALTER TABLE `shop`.`orders` CONVERT TO CHARACTER SET utf8mb4",
                Some("shop.orders"),
            ),
            ("ALTER TABLE orders DEFAULT CHARSET=latin1", Some("orders")),
            ("FOO BAR", None),
        ] {
            assert!(
                matches!(
                    kind(sql),
                    StatementKind::Unparsed { table: ref found, .. } if found.as_deref() == table
                ),
                "{sql}"
            );
        }
    }
}
