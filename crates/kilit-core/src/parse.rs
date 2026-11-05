//! Turns SQL text into kilit's statements, with sqlparser's MySQL dialect.
//!
//! This is the only module that sees sqlparser's types. Anything sqlparser
//! reads but kilit does not model becomes `Other`. Anything it cannot read,
//! anything MySQL 8.0.46 and 8.4.13 do not accept although sqlparser does,
//! and `TRUNCATE` and `LOCK TABLES`, which kilit has not measured, become
//! `Unparsed`, with the reason. Nothing is dropped silently.

use sqlparser::ast::{
    self, AlterColumnOperation, AlterTableAlgorithm, AlterTableLock, AlterTableOperation,
    BinaryOperator, CharacterLength, CheckConstraint, ColumnOption, ContextModifier,
    CreateTableOptions, Expr, ForeignKeyConstraint, Function, FunctionArg, FunctionArgExpr,
    FunctionArguments, GeneratedExpressionMode, Ident, IndexColumn, MySQLColumnPosition,
    ObjectName, ObjectNamePart, ObjectType, ReferentialAction, RenameTableNameKind, Spanned,
    SqlOption, TableConstraint, UnaryOperator, Value,
};
use sqlparser::dialect::MySqlDialect;
use sqlparser::keywords::Keyword;
use sqlparser::parser::{Parser, ParserError};
use sqlparser::tokenizer::{Location, Token, Tokenizer};

use crate::split::split;
use crate::statement::{
    Algorithm, Alter, Check, CheckColumns, Column, DataType, ForeignKey, ForeignKeyAction,
    Generated, Index, IndexKind, IntegerSize, Lock, Operation, Position, Rename, Statement,
    StatementKind, Table, Variable,
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
            Ok([statement]) => convert(statement, sql),
            Err(statements) => StatementKind::Unparsed {
                error: unreadable(format!(
                    "expected one statement, found {}",
                    statements.len()
                )),
                table: table_named_in(sql),
            },
        },
        Err(err) => StatementKind::Unparsed {
            error: unreadable(parser_message(err)),
            table: table_named_in(sql),
        },
    }
}

/// The reason kilit gives no answer for a statement it could not read.
fn unreadable(detail: impl std::fmt::Display) -> String {
    format!("kilit could not read this statement: {detail}")
}

/// The table a statement names, found from the words alone for a statement
/// the parser cannot read: after `ON` in `CREATE [UNIQUE | FULLTEXT |
/// SPATIAL] INDEX` and `DROP INDEX`, such as `orders` in `CREATE FULLTEXT
/// INDEX ft ON orders (note)`, and after the word `TABLE` in the others, such
/// as `orders` in `ALTER TABLE orders CONVERT TO CHARACTER SET utf8mb4`.
fn table_named_in(sql: &str) -> Option<String> {
    let words = words(sql);
    let first: [Keyword; 3] =
        std::array::from_fn(|at| words.get(at).map_or(Keyword::NoKeyword, keyword));
    let before_table = match first {
        [Keyword::CREATE, Keyword::INDEX, _]
        | [
            Keyword::CREATE,
            Keyword::UNIQUE | Keyword::FULLTEXT | Keyword::SPATIAL,
            Keyword::INDEX,
        ]
        | [Keyword::DROP, Keyword::INDEX, _] => Keyword::ON,
        _ => Keyword::TABLE,
    };
    table_after(&words, before_table).map(|(table, _)| table)
}

/// The tokens of `sql` without the spaces and comments between them; none
/// if the tokenizer cannot read it.
fn words(sql: &str) -> Vec<Token> {
    let mut tokens = Tokenizer::new(&MySqlDialect {}, sql)
        .tokenize()
        .unwrap_or_default();
    tokens.retain(|token| !matches!(token, Token::Whitespace(_)));
    tokens
}

/// The keyword `token` is, if it is one. A quoted word is never a keyword.
fn keyword(token: &Token) -> Keyword {
    match token {
        Token::Word(word) => word.keyword,
        _ => Keyword::NoKeyword,
    }
}

/// The table named right after the first `marker` in `words`, such as
/// `shop.orders`, and the words after the name.
fn table_after(words: &[Token], marker: Keyword) -> Option<(String, &[Token])> {
    let start = words.iter().position(|word| keyword(word) == marker)? + 1;
    match &words[start..] {
        [
            Token::Word(schema),
            Token::Period,
            Token::Word(table),
            rest @ ..,
        ] => Some((format!("{}.{}", schema.value, table.value), rest)),
        [Token::Word(table), rest @ ..] => Some((table.value.clone(), rest)),
        _ => None,
    }
}

/// Whether `sql`, a `CREATE INDEX` statement, has `USING` right after the
/// table name, as in `CREATE INDEX i ON t USING BTREE (c)`. The parser reads
/// it there and before `ON` alike; MySQL only before `ON` or after the
/// column list.
fn using_after_table(sql: &str) -> bool {
    let words = words(sql);
    matches!(
        table_after(&words, Keyword::ON),
        Some((_, [next, ..])) if keyword(next) == Keyword::USING
    )
}

fn parser_message(err: ParserError) -> String {
    match err {
        ParserError::TokenizerError(message) | ParserError::ParserError(message) => message,
        ParserError::RecursionLimitExceeded => "the statement is nested too deeply".to_owned(),
    }
}

fn convert(statement: ast::Statement, sql: &str) -> StatementKind {
    // The statement fails on the measured servers or has not been measured:
    // it changes no table definition there, so there is no table to name.
    if let Some(reason) = no_answer(&statement, sql) {
        return StatementKind::Unparsed {
            error: reason,
            table: None,
        };
    }
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

/// How kilit's reasons name what MySQL 8.0.46 and 8.4.13 reject. Each form
/// was run by hand on both servers and failed with error 1064, or 1067 for
/// a date or time literal as a default; the lab has no fact for it yet, so
/// kilit says no more than that.
const REJECTED_BY: &str = "MySQL 8.0.46 and 8.4.13 do not accept";

/// Why kilit gives no answer for `statement`, which sqlparser reads from
/// `sql`: MySQL 8.0.46 and 8.4.13 do not accept it, or kilit has not
/// measured it.
fn no_answer(statement: &ast::Statement, sql: &str) -> Option<String> {
    match statement {
        ast::Statement::Truncate(_) => {
            return Some(
                "truncating a table has not been measured yet; TRUNCATE removes every row"
                    .to_owned(),
            );
        }
        ast::Statement::LockTables { .. } => {
            return Some(
                "LOCK TABLES has not been measured yet; other connections cannot write \
                 to the tables until UNLOCK TABLES"
                    .to_owned(),
            );
        }
        _ => {}
    }
    let rejected = rejected_words(statement, sql);
    if !rejected.is_empty() {
        return Some(format!("{REJECTED_BY} {}", rejected.join(" or ")));
    }
    if table_names(statement).into_iter().any(double_quoted) {
        return Some(
            "a double-quoted table name works only when sql_mode has ANSI_QUOTES, \
             and kilit does not know the server's sql_mode"
                .to_owned(),
        );
    }
    let key_parts = key_parts(statement);
    if names(statement)
        .into_iter()
        .any(|name| name.quote_style == Some('"'))
        || key_parts.iter().any(|part| double_quoted_string(part))
    {
        return Some(
            "a double-quoted column, index or constraint name works only when sql_mode \
             has ANSI_QUOTES, and kilit does not know the server's sql_mode"
                .to_owned(),
        );
    }
    if let Some(default) = rejected_default(statement) {
        return Some(format!("{REJECTED_BY} {default} without parentheses"));
    }
    if let Some(part) = key_parts
        .into_iter()
        .find(|part| matches!(part, Expr::Function(function) if prefix(function).is_none()))
    {
        return Some(format!(
            "{REJECTED_BY} the key part {part}; an expression in an index needs its own \
             parentheses, and a prefix length is a whole number"
        ));
    }
    None
}

/// The words in `statement`, read from `sql`, that MySQL 8.0.46 and 8.4.13
/// do not accept, as kilit names them.
fn rejected_words(statement: &ast::Statement, sql: &str) -> Vec<&'static str> {
    let mut words = Vec::new();
    match statement {
        ast::Statement::AlterTable(alter) => {
            if alter.if_exists {
                words.push("ALTER TABLE IF EXISTS");
            }
            if alter.only {
                words.push("ALTER TABLE ONLY");
            }
            for operation in &alter.operations {
                match operation {
                    AlterTableOperation::DropColumn {
                        if_exists: true, ..
                    } => words.push("DROP COLUMN IF EXISTS"),
                    AlterTableOperation::AddConstraint {
                        not_valid: true, ..
                    } => words.push("NOT VALID"),
                    _ => {}
                }
            }
        }
        ast::Statement::CreateIndex(index) => {
            if index.concurrently {
                words.push("CREATE INDEX CONCURRENTLY");
            }
            if index.if_not_exists {
                words.push("CREATE INDEX IF NOT EXISTS");
            }
            if index.using.is_some() && using_after_table(sql) {
                words.push("USING between the table name and the column list in CREATE INDEX");
            }
            if !index.include.is_empty() {
                words.push("INCLUDE in CREATE INDEX");
            }
            match index.nulls_distinct {
                Some(true) => words.push("NULLS DISTINCT in CREATE INDEX"),
                Some(false) => words.push("NULLS NOT DISTINCT in CREATE INDEX"),
                None => {}
            }
            if index.predicate.is_some() {
                words.push("WHERE in CREATE INDEX");
            }
            if index
                .columns
                .iter()
                .any(|column| column.column.options.nulls_first.is_some())
            {
                words.push("NULLS FIRST or NULLS LAST in an index");
            }
        }
        ast::Statement::Drop {
            object_type: ObjectType::Index,
            if_exists,
            table,
            ..
        } => {
            if table.is_none() {
                words.push("DROP INDEX without ON and a table name");
            } else if *if_exists {
                words.push("DROP INDEX IF EXISTS");
            }
        }
        // LOCK TABLES with READ or WRITE is MySQL's: sqlparser reads it as
        // `LockTables`. Anything else that starts with LOCK lands here.
        ast::Statement::Lock(_) => words.push("LOCK TABLE without READ or WRITE"),
        _ => {}
    }
    words
}

/// The tables `statement` names, where MySQL expects a table name.
fn table_names(statement: &ast::Statement) -> Vec<&ObjectName> {
    match statement {
        // With the table a foreign key points at and the new name.
        ast::Statement::AlterTable(alter) => std::iter::once(&alter.name)
            .chain(
                alter
                    .operations
                    .iter()
                    .filter_map(|operation| match operation {
                        AlterTableOperation::AddConstraint {
                            constraint: TableConstraint::ForeignKey(key),
                            ..
                        } => Some(&key.foreign_table),
                        AlterTableOperation::RenameTable {
                            table_name:
                                RenameTableNameKind::As(name) | RenameTableNameKind::To(name),
                        } => Some(name),
                        _ => None,
                    }),
            )
            .collect(),
        ast::Statement::CreateIndex(index) => vec![&index.table_name],
        ast::Statement::CreateTable(create) => vec![&create.name],
        ast::Statement::Drop {
            table: Some(table), ..
        } => vec![table],
        ast::Statement::Drop {
            object_type: ObjectType::Table,
            names,
            ..
        } => names.iter().collect(),
        ast::Statement::RenameTable(renames) => renames
            .iter()
            .flat_map(|rename| [&rename.old_name, &rename.new_name])
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether `name` is written in double quotes, which MySQL reads as a
/// string unless sql_mode has ANSI_QUOTES.
fn double_quoted(name: &ObjectName) -> bool {
    name.0.iter().any(
        |part| matches!(part, ObjectNamePart::Identifier(ident) if ident.quote_style == Some('"')),
    )
}

/// The column, index and constraint names `statement` writes where MySQL
/// expects a name. The key parts of an index are in [`key_parts`].
fn names(statement: &ast::Statement) -> Vec<&Ident> {
    let mut names = Vec::new();
    match statement {
        ast::Statement::AlterTable(alter) => {
            for operation in &alter.operations {
                operation_names(operation, &mut names);
            }
        }
        ast::Statement::CreateIndex(index) => names.extend(index.name.iter().flat_map(parts)),
        ast::Statement::Drop {
            object_type: ObjectType::Index,
            names: indexes,
            ..
        } => names.extend(indexes.iter().flat_map(parts)),
        _ => {}
    }
    names
}

/// The parts of `shop.orders`.
fn parts(name: &ObjectName) -> impl Iterator<Item = &Ident> {
    name.0.iter().filter_map(|part| match part {
        ObjectNamePart::Identifier(ident) => Some(ident),
        _ => None,
    })
}

fn operation_names<'a>(operation: &'a AlterTableOperation, names: &mut Vec<&'a Ident>) {
    let after = |position: &'a Option<MySQLColumnPosition>| match position {
        Some(MySQLColumnPosition::After(column)) => Some(column),
        _ => None,
    };
    match operation {
        AlterTableOperation::AddConstraint { constraint, .. } => match constraint {
            TableConstraint::Index(index) => names.extend(&index.name),
            TableConstraint::Unique(unique) => {
                names.extend(unique.name.iter().chain(&unique.index_name));
            }
            TableConstraint::PrimaryKey(key) => {
                names.extend(key.name.iter().chain(&key.index_name))
            }
            TableConstraint::FulltextOrSpatial(index) => names.extend(&index.opt_index_name),
            TableConstraint::ForeignKey(key) => names.extend(
                key.name
                    .iter()
                    .chain(&key.index_name)
                    .chain(&key.columns)
                    .chain(&key.referred_columns),
            ),
            TableConstraint::Check(check) => names.extend(&check.name),
            _ => {}
        },
        AlterTableOperation::AddColumn {
            column_def,
            column_position,
            ..
        } => {
            names.push(&column_def.name);
            names.extend(
                column_def
                    .options
                    .iter()
                    .filter_map(|option| option.name.as_ref()),
            );
            names.extend(after(column_position));
        }
        AlterTableOperation::ModifyColumn {
            col_name,
            column_position,
            ..
        } => {
            names.push(col_name);
            names.extend(after(column_position));
        }
        AlterTableOperation::ChangeColumn {
            old_name,
            new_name,
            column_position,
            ..
        } => {
            names.extend([old_name, new_name]);
            names.extend(after(column_position));
        }
        AlterTableOperation::RenameColumn {
            old_column_name,
            new_column_name,
        } => names.extend([old_column_name, new_column_name]),
        AlterTableOperation::AlterColumn { column_name, .. } => names.push(column_name),
        AlterTableOperation::DropColumn { column_names, .. } => names.extend(column_names),
        AlterTableOperation::DropIndex { name }
        | AlterTableOperation::DropForeignKey { name, .. }
        | AlterTableOperation::DropConstraint { name, .. } => names.push(name),
        _ => {}
    }
}

/// The key parts of the indexes and keys `statement` adds, such as `amount`
/// and `note(10)` in `ADD INDEX i (amount, note(10))`.
fn key_parts(statement: &ast::Statement) -> Vec<&Expr> {
    let columns: Vec<&IndexColumn> = match statement {
        ast::Statement::AlterTable(alter) => alter
            .operations
            .iter()
            .flat_map(|operation| match operation {
                AlterTableOperation::AddConstraint { constraint, .. } => match constraint {
                    TableConstraint::Index(index) => index.columns.as_slice(),
                    TableConstraint::Unique(unique) => unique.columns.as_slice(),
                    TableConstraint::PrimaryKey(key) => key.columns.as_slice(),
                    TableConstraint::FulltextOrSpatial(index) => index.columns.as_slice(),
                    _ => &[],
                },
                _ => &[],
            })
            .collect(),
        ast::Statement::CreateIndex(index) => index.columns.iter().collect(),
        _ => Vec::new(),
    };
    columns
        .into_iter()
        .map(|column| &column.column.expr)
        .collect()
}

/// `"amount"` as a key part: the parser reads it as a string, as MySQL does
/// without ANSI_QUOTES.
fn double_quoted_string(part: &Expr) -> bool {
    matches!(part, Expr::Value(value) if matches!(value.value, Value::DoubleQuotedString(_)))
}

/// The first default in `statement` that MySQL 8.0.46 and 8.4.13 do not
/// take without parentheses, as kilit names it: `DEFAULT CURRENT_DATE` or
/// `SET DEFAULT CURRENT_TIMESTAMP`.
fn rejected_default(statement: &ast::Statement) -> Option<String> {
    let ast::Statement::AlterTable(alter) = statement else {
        return None;
    };
    alter
        .operations
        .iter()
        .find_map(|operation| match operation {
            AlterTableOperation::AddColumn { column_def, .. } => column_def
                .options
                .iter()
                .find_map(|option| rejected_column_default(&option.option)),
            AlterTableOperation::ModifyColumn { options, .. }
            | AlterTableOperation::ChangeColumn { options, .. } => {
                options.iter().find_map(rejected_column_default)
            }
            AlterTableOperation::AlterColumn {
                op: AlterColumnOperation::SetDefault { value },
                ..
            } => (!plain_default(value, false)).then(|| format!("SET DEFAULT {value}")),
            _ => None,
        })
}

fn rejected_column_default(option: &ColumnOption) -> Option<String> {
    match option {
        ColumnOption::Default(value) if !plain_default(value, true) => {
            Some(format!("DEFAULT {value}"))
        }
        _ => None,
    }
}

/// Whether MySQL takes the default `value` as written: in parentheses, a
/// literal, or, in a column definition (`now`), CURRENT_TIMESTAMP and its
/// synonyms. `ALTER COLUMN ... SET DEFAULT` takes no CURRENT_TIMESTAMP.
/// Every form, and the ones refused, was run by hand on MySQL 8.0.46 and
/// 8.4.13, with the same result on both.
fn plain_default(value: &Expr, now: bool) -> bool {
    match value {
        // An expression default, which `check` answers.
        Expr::Nested(_) => true,
        Expr::Value(value) => matches!(
            value.value,
            Value::Number(..)
                | Value::SingleQuotedString(_)
                | Value::DoubleQuotedString(_)
                | Value::NationalStringLiteral(_)
                | Value::HexStringLiteral(_)
                | Value::SingleQuotedByteStringLiteral(_)
                | Value::Boolean(_)
                | Value::Null
        ),
        // `-1`, `+1.5`
        Expr::UnaryOp {
            op: UnaryOperator::Plus | UnaryOperator::Minus,
            expr,
        } => matches!(&**expr, Expr::Value(value) if matches!(value.value, Value::Number(..))),
        // `_utf8mb4'x'`, `_binary x'0A'`
        Expr::Prefixed { prefix, value } => {
            prefix.value.starts_with('_')
                && matches!(&**value, Expr::Value(value) if matches!(
                    value.value,
                    Value::SingleQuotedString(_)
                        | Value::DoubleQuotedString(_)
                        | Value::HexStringLiteral(_)
                ))
        }
        // `0b01`, which the parser reads as a name.
        Expr::Identifier(name) => {
            name.quote_style.is_none()
                && name.value.strip_prefix("0b").is_some_and(|bits| {
                    !bits.is_empty() && bits.bytes().all(|bit| matches!(bit, b'0' | b'1'))
                })
        }
        Expr::Function(function) => now && current_timestamp(function),
        _ => false,
    }
}

/// `CURRENT_TIMESTAMP`, `NOW()`, `LOCALTIME` or `LOCALTIMESTAMP`, with
/// nothing, `()` or a whole number in parentheses after it.
fn current_timestamp(function: &Function) -> bool {
    let [ObjectNamePart::Identifier(name)] = function.name.0.as_slice() else {
        return false;
    };
    let synonym = name.quote_style.is_none()
        && ["CURRENT_TIMESTAMP", "NOW", "LOCALTIME", "LOCALTIMESTAMP"]
            .iter()
            .any(|synonym| name.value.eq_ignore_ascii_case(synonym));
    let precision = match &function.args {
        FunctionArguments::None => true,
        FunctionArguments::List(list) => {
            list.duplicate_treatment.is_none()
                && list.clauses.is_empty()
                && match list.args.as_slice() {
                    [] => true,
                    [precision] => whole_number(precision).is_some(),
                    _ => false,
                }
        }
        FunctionArguments::Subquery(_) => false,
    };
    synonym && precision && plain_call(function)
}

/// A call with nothing around its arguments: no `FILTER`, `OVER` or the like.
fn plain_call(function: &Function) -> bool {
    matches!(function.parameters, FunctionArguments::None)
        && function.filter.is_none()
        && function.null_treatment.is_none()
        && function.over.is_none()
        && function.within_group.is_empty()
        && !function.uses_odbc_syntax
}

/// The digits of `argument` when it is a whole number, such as `10`.
fn whole_number(argument: &FunctionArg) -> Option<&str> {
    let FunctionArg::Unnamed(FunctionArgExpr::Expr(Expr::Value(value))) = argument else {
        return None;
    };
    match &value.value {
        Value::Number(digits, _)
            if !digits.is_empty() && digits.bytes().all(|digit| digit.is_ascii_digit()) =>
        {
            Some(digits)
        }
        _ => None,
    }
}

/// The column and length of a prefix key part, such as `note` and `10` in
/// `note(10)` or `` `note`(10) ``, which the parser reads as a call.
fn prefix(function: &Function) -> Option<(&Ident, &str)> {
    let [ObjectNamePart::Identifier(column)] = function.name.0.as_slice() else {
        return None;
    };
    let FunctionArguments::List(list) = &function.args else {
        return None;
    };
    let [length] = list.args.as_slice() else {
        return None;
    };
    if list.duplicate_treatment.is_some() || !list.clauses.is_empty() || !plain_call(function) {
        return None;
    }
    whole_number(length).map(|length| (column, length))
}

/// `CREATE TABLE` with its columns, keys and default character set.
fn create_table(create: ast::CreateTable) -> StatementKind {
    let name = object_name(&create.name);
    if create.like.is_some() || create.query.is_some() || create.clone.is_some() {
        return StatementKind::Unparsed {
            error: unreadable(
                "the table takes its columns from elsewhere (LIKE or AS SELECT), \
                 which kilit does not follow yet",
            ),
            table: Some(name),
        };
    }
    let mut table = Table {
        name,
        ..Table::default()
    };
    // With where each one is written: see below.
    let mut checks = Vec::new();
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
                ColumnOption::ForeignKey(_) => {}
                ColumnOption::Check(check) => checks.push(written_check(CheckConstraint {
                    name: option.name,
                    ..check
                })),
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
            TableConstraint::Check(check) => checks.push(written_check(check)),
            other => {
                if let Operation::AddIndex(index) = added_constraint(other) {
                    table.indexes.push(index);
                }
            }
        }
    }
    // MySQL numbers the CHECKs it names in the order they are written, and the
    // parser keeps the ones on columns apart from the others.
    checks.sort_by_key(|(written_at, _)| *written_at);
    table.checks = checks.into_iter().map(|(_, check)| check).collect();
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
        // MySQL 8.0.46 and 8.4.13 do not accept the other forms: IF EXISTS,
        // CASCADE and RESTRICT stay something kilit does not understand.
        AlterTableOperation::DropConstraint {
            if_exists: false,
            name,
            drop_behavior: None,
        } => Operation::DropConstraint { name: name.value },
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
        // MySQL 8.0.46 and 8.4.13 do not accept NO INHERIT, which stays
        // something kilit does not understand.
        TableConstraint::Check(check) if !check.no_inherit => {
            Operation::AddCheck(check_constraint(check))
        }
        other => Operation::Other {
            sql: format!("ADD {other}"),
        },
    }
}

fn check_constraint(check: CheckConstraint) -> Check {
    let expression = check.expr.to_string();
    Check {
        name: check.name.map(|name| name.value),
        columns: check_columns(&check.expr, &expression),
        expression,
        enforced: check.enforced != Some(false),
    }
}

/// A CHECK of a `CREATE TABLE`, with where it starts in the statement.
fn written_check(check: CheckConstraint) -> (Location, Check) {
    (check.expr.span().start, check_constraint(check))
}

/// The columns a CHECK's expression uses, or else every name it has outside a
/// string: in MySQL a column has to be named to be used.
fn check_columns(expression: &Expr, printed: &str) -> CheckColumns {
    let mut columns = Vec::new();
    if boolean_expression(expression) && plain_expression(expression, &mut columns) {
        CheckColumns::Plain(columns)
    } else {
        CheckColumns::Other(names_outside_strings(printed))
    }
}

/// Every name in `printed`, an expression as the parser prints it. Without
/// ANSI_QUOTES a double-quoted word is a string, with it a name, and kilit does
/// not know the server's sql_mode. The parser prints a string without its
/// escapes, so `'a\\'` does not read back the same way: when one may be in there,
/// every word counts, in a string too, and every name in backticks.
fn names_outside_strings(printed: &str) -> Vec<String> {
    let tokens = if printed.contains('\\') {
        None
    } else {
        Tokenizer::new(&MySqlDialect {}, printed).tokenize().ok()
    };
    let Some(tokens) = tokens else {
        let words = printed.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'));
        let quoted = printed.split('`').skip(1).step_by(2);
        return words
            .chain(quoted)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
    };
    tokens
        .into_iter()
        .filter_map(|token| match token {
            Token::Word(word) => Some(word.value),
            Token::DoubleQuotedString(text) => Some(text),
            _ => None,
        })
        .collect()
}

/// Whether the expression gives a truth value at its top: MySQL refuses
/// `CHECK (amount)` with error 3812, run by hand on 8.0.46 and 8.4.13.
fn boolean_expression(expression: &Expr) -> bool {
    match expression {
        Expr::Nested(inner) => boolean_expression(inner),
        Expr::IsNull(_)
        | Expr::IsNotNull(_)
        | Expr::UnaryOp {
            op: UnaryOperator::Not,
            ..
        } => true,
        Expr::BinaryOp { op, .. } => matches!(
            op,
            BinaryOperator::And
                | BinaryOperator::Or
                | BinaryOperator::Eq
                | BinaryOperator::NotEq
                | BinaryOperator::Lt
                | BinaryOperator::LtEq
                | BinaryOperator::Gt
                | BinaryOperator::GtEq
        ),
        _ => false,
    }
}

/// Whether the expression has nothing but column names, numbers, arithmetic
/// and comparison operators, `AND`, `OR`, `NOT`, `IS [NOT] NULL` and
/// parentheses. Adds the columns it names to `columns`.
fn plain_expression(expression: &Expr, columns: &mut Vec<String>) -> bool {
    match expression {
        Expr::Identifier(column) => {
            columns.push(column.value.clone());
            true
        }
        Expr::Value(value) => matches!(value.value, Value::Number(..)),
        Expr::Nested(inner) | Expr::IsNull(inner) | Expr::IsNotNull(inner) => {
            plain_expression(inner, columns)
        }
        Expr::UnaryOp {
            op: UnaryOperator::Not | UnaryOperator::Plus | UnaryOperator::Minus,
            expr,
        } => plain_expression(expr, columns),
        Expr::BinaryOp { left, op, right } => {
            matches!(
                op,
                BinaryOperator::Plus
                    | BinaryOperator::Minus
                    | BinaryOperator::Multiply
                    | BinaryOperator::Divide
                    | BinaryOperator::Modulo
                    | BinaryOperator::And
                    | BinaryOperator::Or
                    | BinaryOperator::Eq
                    | BinaryOperator::NotEq
                    | BinaryOperator::Lt
                    | BinaryOperator::LtEq
                    | BinaryOperator::Gt
                    | BinaryOperator::GtEq
            ) && plain_expression(left, columns)
                && plain_expression(right, columns)
        }
        _ => false,
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

/// A plain column keeps its name without quotes, and so does a prefix:
/// `` `email`(10) ``, as a schema dump writes it, becomes `email(10)`. An
/// expression such as `(lower(email))` is kept as the parser prints it.
fn index_column(column: &IndexColumn) -> String {
    match &column.column.expr {
        Expr::Identifier(ident) => ident.value.clone(),
        Expr::Function(function) => match prefix(function) {
            Some((column, length)) => format!("{}({length})", column.value),
            None => function.to_string(),
        },
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
/// The exception is a global `sql_log_bin`, which MySQL refuses: kilit
/// keeps it, so that the statements after it are not judged as if nothing
/// had touched the binary log. A statement with none of these becomes
/// `Other`.
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
            let (name, global) = match session_variable(scope, &name) {
                Some(name) => (name, false),
                None if global_variable(scope, &name).as_deref() == Some("sql_log_bin") => {
                    ("sql_log_bin".to_owned(), true)
                }
                None => return None,
            };
            Some(Variable {
                name,
                value: value.to_string(),
                global,
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

/// The lowercase name of a global variable: `GLOBAL x` or `@@global.x`.
fn global_variable(scope: Option<ContextModifier>, name: &ObjectName) -> Option<String> {
    let name = object_name(name).to_lowercase();
    if scope == Some(ContextModifier::Global) {
        return Some(name);
    }
    name.strip_prefix("@@global.").map(str::to_owned)
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
    fn expression_defaults_keep_their_parentheses() {
        // MySQL takes an expression as a default only in parentheses, so
        // they tell it from CURRENT_TIMESTAMP and from a literal.
        for (sql, default) in [
            (
                "ALTER TABLE t ADD a TIMESTAMP NULL DEFAULT CURRENT_TIMESTAMP",
                "CURRENT_TIMESTAMP",
            ),
            (
                "ALTER TABLE t ADD a TIMESTAMP NULL DEFAULT (CURRENT_TIMESTAMP)",
                "(CURRENT_TIMESTAMP)",
            ),
            (
                "ALTER TABLE t ADD a CHAR(36) NOT NULL DEFAULT (UUID())",
                "(UUID())",
            ),
            (
                "ALTER TABLE t ADD a DOUBLE NOT NULL DEFAULT ( rand( ) )",
                "(rand())",
            ),
            (
                "ALTER TABLE t ADD a DATE NOT NULL DEFAULT (current_date + interval 1 year)",
                "(current_date + INTERVAL 1 YEAR)",
            ),
            ("ALTER TABLE t ADD a INT NOT NULL DEFAULT (0)", "(0)"),
            ("ALTER TABLE t ADD a INT NOT NULL DEFAULT 0", "0"),
        ] {
            assert_eq!(added_column(sql).default.as_deref(), Some(default), "{sql}");
        }
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
    fn an_expression_in_an_index_keeps_its_parentheses() {
        // sqlparser reads the prefix `note(10)` as a call of `note`; only an
        // expression starts with a parenthesis, as MySQL requires.
        for (sql, columns) in [
            (
                "ALTER TABLE t ADD INDEX i ((LOWER(email)))",
                vec!["(LOWER(email))"],
            ),
            (
                "alter table `t` add index `i`((lower(email)))",
                vec!["(lower(email))"],
            ),
            (
                "CREATE INDEX i ON t ((LOWER(email)))",
                vec!["(LOWER(email))"],
            ),
            ("ALTER TABLE t ADD INDEX i (note(10))", vec!["note(10)"]),
            ("ALTER TABLE t ADD INDEX i ((note(10)))", vec!["(note(10))"]),
            (
                "ALTER TABLE t ADD INDEX i (user_id, (LOWER(email)))",
                vec!["user_id", "(LOWER(email))"],
            ),
        ] {
            let StatementKind::Alter(alter) = kind(sql) else {
                panic!("expected an ALTER: {sql}");
            };
            let [Operation::AddIndex(index)] = alter.operations.as_slice() else {
                panic!("expected ADD INDEX: {sql}");
            };
            assert_eq!(index.columns, columns, "{sql}");
        }
    }

    #[test]
    fn a_prefix_names_its_column_without_quotes() {
        // As a schema dump writes it, and in other spellings MySQL 8.0.46 and
        // 8.4.13 accept (run by hand).
        let Operation::AddIndex(index) =
            operation("ALTER TABLE k ADD INDEX k7 (`note`(10), NOTE(5), note (8), `note`(010))")
        else {
            panic!("expected ADD INDEX");
        };
        assert_eq!(
            index.columns,
            ["note(10)", "NOTE(5)", "note(8)", "note(010)"]
        );
        let table = created(
            "CREATE TABLE `codes` (
               `code` varchar(50) NOT NULL,
               `label` varchar(20) DEFAULT NULL,
               PRIMARY KEY (`code`(10)),
               KEY `codes_label_index` (`label`(5))
             )",
        );
        assert_eq!(table.primary_key, ["code(10)"]);
        assert_eq!(table.indexes[0].columns, ["label(5)"]);
        // An expression keeps its quotes, for the words it uses.
        let Operation::AddIndex(index) = operation("ALTER TABLE k ADD INDEX k8 ((lower(`email`)))")
        else {
            panic!("expected ADD INDEX");
        };
        assert_eq!(index.columns, ["(lower(`email`))"]);
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
                global: false,
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
                        global: false,
                    },
                    Variable {
                        name: "unique_checks".into(),
                        value: "0".into(),
                        global: false,
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
                    global: false,
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
    fn a_global_sql_log_bin_is_kept() {
        // MySQL refuses it; kilit keeps it so the statements after it are
        // not judged as if nothing had touched the binary log.
        for sql in [
            "SET GLOBAL sql_log_bin = 0",
            "SET @@global.sql_log_bin = 0",
            "SET @@GLOBAL.SQL_LOG_BIN = 0",
        ] {
            assert_eq!(
                kind(sql),
                StatementKind::Set {
                    variables: vec![Variable {
                        name: "sql_log_bin".into(),
                        value: "0".into(),
                        global: true,
                    }]
                },
                "{sql}"
            );
        }
        assert_eq!(
            kind("SET SESSION sql_log_bin = OFF"),
            StatementKind::Set {
                variables: vec![Variable {
                    name: "sql_log_bin".into(),
                    value: "OFF".into(),
                    global: false,
                }]
            }
        );
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
        // MySQL 8.0.46 and 8.4.13 do not accept these forms (run by hand).
        for (sql, printed) in [
            (
                "ALTER TABLE t ADD CHECK (a > 0) NO INHERIT",
                "ADD CHECK (a > 0) NO INHERIT",
            ),
            (
                "ALTER TABLE t DROP CONSTRAINT IF EXISTS c",
                "DROP CONSTRAINT IF EXISTS c",
            ),
            (
                "ALTER TABLE t DROP CONSTRAINT c CASCADE",
                "DROP CONSTRAINT c CASCADE",
            ),
            (
                "ALTER TABLE t DROP CONSTRAINT c RESTRICT",
                "DROP CONSTRAINT c RESTRICT",
            ),
        ] {
            assert_eq!(
                operation(sql),
                Operation::Other {
                    sql: printed.into()
                },
                "{sql}"
            );
        }
    }

    /// The CHECK constraints of a `CREATE TABLE`, in the order MySQL numbers them.
    fn checks_of(sql: &str) -> Vec<Check> {
        match kind(sql) {
            StatementKind::CreateTable { table, .. } => table.checks,
            other => panic!("{sql}: expected a CREATE TABLE, got {other:?}"),
        }
    }

    fn plain(columns: &[&str]) -> CheckColumns {
        CheckColumns::Plain(columns.iter().map(|&column| column.to_owned()).collect())
    }

    #[test]
    fn reads_check_constraints_in_alter_table() {
        let check = |sql: &str| match operation(sql) {
            Operation::AddCheck(check) => check,
            other => panic!("{sql}: expected ADD CHECK, got {other:?}"),
        };
        assert_eq!(
            check("ALTER TABLE t ADD CONSTRAINT `c` CHECK (a > 0)"),
            Check {
                name: Some("c".into()),
                expression: "a > 0".into(),
                enforced: true,
                columns: plain(&["a"]),
            }
        );
        // Without a name, in both spellings, and with ENFORCED written.
        for sql in [
            "ALTER TABLE t ADD CHECK (a > 0)",
            "ALTER TABLE t ADD CONSTRAINT CHECK (a > 0)",
            "ALTER TABLE t ADD CHECK (a > 0) ENFORCED",
        ] {
            let found = check(sql);
            assert_eq!(found.name, None, "{sql}");
            assert!(found.enforced, "{sql}");
        }
        assert!(!check("ALTER TABLE t ADD CHECK (a > 0) NOT ENFORCED").enforced);
        assert_eq!(
            operation("ALTER TABLE t DROP CONSTRAINT `c`"),
            Operation::DropConstraint { name: "c".into() }
        );
    }

    #[test]
    fn check_constraints_sqlparser_cannot_read_stay_unparsed() {
        // All valid MySQL 8.0.46 and 8.4.13 (run by hand). sqlparser 0.63 takes
        // the word after DROP or ALTER for a column's name.
        for sql in [
            "ALTER TABLE t DROP CHECK c",
            "ALTER TABLE t ALTER CHECK c NOT ENFORCED",
            "ALTER TABLE t ALTER CONSTRAINT c ENFORCED",
        ] {
            assert!(
                matches!(
                    kind(sql),
                    StatementKind::Unparsed { ref table, .. } if table.as_deref() == Some("t")
                ),
                "{sql}"
            );
        }
    }

    #[test]
    fn a_table_keeps_its_check_constraints_as_a_schema_dump_writes_them() {
        let checks = checks_of(
            "CREATE TABLE `orders` (
               `id` bigint unsigned NOT NULL,
               `AMOUNT` int NOT NULL,
               `note` varchar(50) DEFAULT NULL,
               CONSTRAINT `orders_chk_1` CHECK ((`AMOUNT` > `id`)),
               CONSTRAINT `orders_chk_2` CHECK ((`amount` > 0)) /*!80016 NOT ENFORCED */,
               CONSTRAINT `orders_chk_3` CHECK ((`note` <> _utf8mb4'x'))
             ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        );
        assert_eq!(
            checks,
            [
                Check {
                    name: Some("orders_chk_1".into()),
                    expression: "(`AMOUNT` > `id`)".into(),
                    enforced: true,
                    columns: plain(&["AMOUNT", "id"]),
                },
                Check {
                    name: Some("orders_chk_2".into()),
                    expression: "(`amount` > 0)".into(),
                    enforced: false,
                    columns: plain(&["amount"]),
                },
                Check {
                    name: Some("orders_chk_3".into()),
                    expression: "(`note` <> _utf8mb4 'x')".into(),
                    enforced: true,
                    columns: CheckColumns::Other(vec!["note".into(), "_utf8mb4".into()]),
                },
            ]
        );
    }

    #[test]
    fn check_constraints_stay_in_the_order_written() {
        // MySQL 8.0.46 and 8.4.13 number them in this order (run by hand), and
        // the parser keeps the ones on columns apart from the others.
        let checks = checks_of(
            "CREATE TABLE t (
               a INT, CHECK (a > 1),
               b INT CHECK (b > 2),
               c INT CONSTRAINT named CHECK (c > 3) NOT ENFORCED,
               d INT, CONSTRAINT CHECK (d > 4),
               e INT CHECK (char_length(e) > 5),
               CONSTRAINT last CHECK (a > b)
             )",
        );
        let found: Vec<(Option<&str>, &str, bool)> = checks
            .iter()
            .map(|check| {
                (
                    check.name.as_deref(),
                    check.expression.as_str(),
                    check.enforced,
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                (None, "a > 1", true),
                (None, "b > 2", true),
                (Some("named"), "c > 3", false),
                (None, "d > 4", true),
                (None, "char_length(e) > 5", true),
                (Some("last"), "a > b", true),
            ]
        );
    }

    #[test]
    fn a_check_is_plain_with_column_names_numbers_and_the_listed_operators() {
        for (expression, columns) in [
            ("a > 0", &["a"][..]),
            ("a = 1", &["a"]),
            ("a <> 1", &["a"]),
            ("a != 1", &["a"]),
            ("a < 1", &["a"]),
            ("a <= 1", &["a"]),
            ("a >= 1", &["a"]),
            ("1.5 < a", &["a"]),
            ("a > 0 AND b > 0", &["a", "b"]),
            ("a > 0 OR b > 0", &["a", "b"]),
            ("NOT (a > b)", &["a", "b"]),
            ("a IS NULL", &["a"]),
            ("a IS NOT NULL", &["a"]),
            ("a IS NULL OR b > 0", &["a", "b"]),
            ("a + b * 2 - 1 > 0", &["a", "b"]),
            ("a / 2 % 3 > 0", &["a"]),
            ("-a < +b", &["a", "b"]),
            ("((`a` > `b`))", &["a", "b"]),
            // As SHOW CREATE TABLE writes them on MySQL 8.0.46 and 8.4.13.
            ("(`a` >= -(1))", &["a"]),
            ("((`a` is null) or (`b` > 0))", &["a", "b"]),
            (
                "((`a` is not null) and ((`a` + (`b` * 2)) >= -(1)))",
                &["a", "a", "b"],
            ),
            ("(((`a` <> 0) and (`a` <> 1)))", &["a", "a"]),
            ("(not(`a`))", &["a"]),
            // Capitals stay as written; a comparison looks past them.
            ("`AMOUNT` > User_Id", &["AMOUNT", "User_Id"]),
            ("a > 0 AND a < 9", &["a", "a"]),
            ("1 > 0", &[]),
        ] {
            let checks = checks_of(&format!(
                "CREATE TABLE t (a INT, b INT, CHECK ({expression}))"
            ));
            assert_eq!(checks[0].columns, plain(columns), "{expression}");
        }
    }

    #[test]
    fn any_other_check_lists_the_names_it_has_outside_strings() {
        let names = |expression: &str| {
            let checks = checks_of(&format!(
                "CREATE TABLE t (a INT, b INT, CHECK ({expression}))"
            ));
            match checks[0].columns.clone() {
                CheckColumns::Other(names) => names,
                CheckColumns::Plain(columns) => panic!("{expression} is plain: {columns:?}"),
            }
        };
        assert_eq!(names("a IN ('a', 'b')"), ["a", "IN"]);
        assert_eq!(names("a IN (b, 2)"), ["a", "IN", "b"]);
        assert_eq!(names("char_length(`a`) > 0"), ["char_length", "a"]);
        assert_eq!(names("a LIKE 'b%'"), ["a", "LIKE"]);
        assert_eq!(names("a NOT LIKE 'b%'"), ["a", "NOT", "LIKE"]);
        assert_eq!(names("a BETWEEN 1 AND 2"), ["a", "BETWEEN", "AND"]);
        // A column named only in a string is not among them.
        assert_eq!(names("a <> 'b'"), ["a"]);
        assert_eq!(names("`a` <> _utf8mb4'b'"), ["a", "_utf8mb4"]);
        // With ANSI_QUOTES this one is a column's name.
        assert_eq!(names("\"a\" > 0"), ["a"]);
        // A string with a backslash may not read back the same way: every word
        // counts then, in a string too, and every name in backticks.
        assert_eq!(names(r"a <> 'b\\'"), ["a", "b"]);
        assert_eq!(
            names(r"`a b` <> 'c\\' OR d <> 'e\\'"),
            ["a", "b", "c", "OR", "d", "e", "a b"]
        );
        // Not a truth value, which MySQL refuses, and operators left out.
        assert_eq!(names("a"), ["a"]);
        assert_eq!(names("a + 1"), ["a"]);
        assert_eq!(names("a <=> b"), ["a", "b"]);
        assert_eq!(names("a DIV 2 > 0"), ["a", "DIV"]);
        assert_eq!(names("a > 0 XOR b > 0"), ["a", "XOR", "b"]);
        assert_eq!(names("a > 0 || b > 0"), ["a", "b"]);
        assert_eq!(names("a > 0 IS TRUE"), ["a", "IS", "TRUE"]);
        assert_eq!(names("a <> NULL"), ["a", "NULL"]);
        assert_eq!(names("t.a > 0"), ["t", "a"]);
        assert_eq!(names("a > CURRENT_TIMESTAMP"), ["a", "CURRENT_TIMESTAMP"]);
    }

    #[test]
    fn a_check_written_on_a_column_of_alter_table_stays_an_option_kilit_does_not_follow() {
        let added = added_column("ALTER TABLE t ADD c INT CHECK (c > 0)");
        assert_eq!(added.other_options, ["CHECK (c > 0)"]);
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
                error: "kilit could not read this statement: the statement is nested too deeply"
                    .into(),
                table: Some("t".into()),
            }
        );
    }

    /// Why kilit gives no answer for the only statement of `sql`, which
    /// changes no table definition on the measured servers.
    fn no_answer_for(sql: &str) -> String {
        match kind(sql) {
            StatementKind::Unparsed { error, table: None } => error,
            other => panic!("{sql}: expected no answer, got {other:?}"),
        }
    }

    #[test]
    fn words_mysql_does_not_accept_get_no_answer() {
        // Each failed with error 1064 on MySQL 8.0.46 and 8.4.13, run by hand.
        for (sql, words) in [
            (
                "ALTER TABLE IF EXISTS orders ADD COLUMN coupon INT",
                "ALTER TABLE IF EXISTS",
            ),
            (
                "ALTER TABLE ONLY orders ADD COLUMN coupon INT NULL",
                "ALTER TABLE ONLY",
            ),
            (
                "ALTER TABLE orders DROP COLUMN IF EXISTS note",
                "DROP COLUMN IF EXISTS",
            ),
            (
                "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) \
                 REFERENCES users (id) NOT VALID",
                "NOT VALID",
            ),
            (
                "CREATE INDEX IF NOT EXISTS i ON orders (amount)",
                "CREATE INDEX IF NOT EXISTS",
            ),
            (
                "CREATE INDEX CONCURRENTLY IF NOT EXISTS i ON orders (amount)",
                "CREATE INDEX CONCURRENTLY or CREATE INDEX IF NOT EXISTS",
            ),
            (
                "CREATE INDEX i ON orders (amount) WHERE amount > 0",
                "WHERE in CREATE INDEX",
            ),
            (
                "CREATE INDEX i ON orders (amount) INCLUDE (user_id)",
                "INCLUDE in CREATE INDEX",
            ),
            (
                "CREATE UNIQUE INDEX i ON orders (amount) NULLS NOT DISTINCT",
                "NULLS NOT DISTINCT in CREATE INDEX",
            ),
            (
                "CREATE UNIQUE INDEX i ON orders (amount) NULLS DISTINCT",
                "NULLS DISTINCT in CREATE INDEX",
            ),
            (
                "CREATE INDEX i ON orders (amount DESC NULLS LAST)",
                "NULLS FIRST or NULLS LAST in an index",
            ),
            (
                "CREATE INDEX i ON orders (amount NULLS FIRST)",
                "NULLS FIRST or NULLS LAST in an index",
            ),
            (
                "DROP INDEX IF EXISTS orders_status_index ON orders",
                "DROP INDEX IF EXISTS",
            ),
            (
                "DROP INDEX orders_status_index",
                "DROP INDEX without ON and a table name",
            ),
            (
                "LOCK TABLE orders IN ACCESS EXCLUSIVE MODE",
                "LOCK TABLE without READ or WRITE",
            ),
        ] {
            assert_eq!(
                no_answer_for(sql),
                format!("MySQL 8.0.46 and 8.4.13 do not accept {words}"),
                "{sql}"
            );
        }
    }

    #[test]
    fn a_double_quoted_table_name_gets_no_answer() {
        for sql in [
            r#"ALTER TABLE "orders" ADD COLUMN coupon INT"#,
            r#"CREATE INDEX i ON "orders" (amount)"#,
            r#"DROP INDEX orders_status_index ON "orders""#,
            r#"CREATE TABLE "drafts" (id INT PRIMARY KEY)"#,
            r#"DROP TABLE "shops""#,
            r#"RENAME TABLE "shops" TO stores"#,
            r#"RENAME TABLE shops TO "stores""#,
        ] {
            assert_eq!(
                no_answer_for(sql),
                "a double-quoted table name works only when sql_mode has ANSI_QUOTES, \
                 and kilit does not know the server's sql_mode",
                "{sql}"
            );
        }
    }

    // The forms in the tests below were run by hand on MySQL 8.0.46 and
    // 8.4.13, with the same result on both. Each default was run in ADD
    // COLUMN; MODIFY and CHANGE take the same column definition.

    #[test]
    fn a_double_quoted_column_index_or_constraint_name_gets_no_answer() {
        // Error 1064 without ANSI_QUOTES: "x" is a string there.
        for sql in [
            r#"ALTER TABLE orders ADD COLUMN "coupon" INT NULL"#,
            r#"ALTER TABLE orders ADD INDEX "ix_amount" (amount)"#,
            r#"ALTER TABLE orders ADD INDEX ix_amount ("amount")"#,
            r#"ALTER TABLE orders ADD UNIQUE INDEX uq_email ("email")"#,
            r#"ALTER TABLE orders ADD FULLTEXT INDEX ft_note ("note")"#,
            r#"ALTER TABLE orders ADD PRIMARY KEY ("id")"#,
            r#"ALTER TABLE orders DROP COLUMN "note""#,
            r#"ALTER TABLE orders MODIFY "note" VARCHAR(60) NULL"#,
            r#"ALTER TABLE orders CHANGE "note" memo VARCHAR(60) NULL"#,
            r#"ALTER TABLE orders CHANGE note "memo" VARCHAR(60) NULL"#,
            r#"ALTER TABLE orders RENAME COLUMN "note" TO memo"#,
            r#"ALTER TABLE orders RENAME COLUMN note TO "memo""#,
            r#"ALTER TABLE orders ALTER COLUMN "amount" SET DEFAULT 0"#,
            r#"ALTER TABLE orders ADD COLUMN coupon INT NULL AFTER "amount""#,
            r#"ALTER TABLE orders DROP INDEX "ix""#,
            r#"ALTER TABLE orders ADD CONSTRAINT "fk_user" FOREIGN KEY (user_id) REFERENCES users (id)"#,
            r#"ALTER TABLE orders ADD CONSTRAINT fk_user FOREIGN KEY ("user_id") REFERENCES users (id)"#,
            r#"ALTER TABLE orders ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES users ("id")"#,
            r#"ALTER TABLE orders DROP FOREIGN KEY "fk_x""#,
            r#"CREATE INDEX "ix_amount" ON orders (amount)"#,
            r#"CREATE INDEX ix_amount ON orders ("amount")"#,
            r#"DROP INDEX "ix" ON orders"#,
        ] {
            assert_eq!(
                no_answer_for(sql),
                "a double-quoted column, index or constraint name works only when sql_mode \
                 has ANSI_QUOTES, and kilit does not know the server's sql_mode",
                "{sql}"
            );
        }
        // The table a foreign key points at and the new name are table names.
        for sql in [
            r#"ALTER TABLE orders ADD CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES "users" (id)"#,
            r#"ALTER TABLE orders RENAME TO "orders2""#,
        ] {
            assert_eq!(
                no_answer_for(sql),
                "a double-quoted table name works only when sql_mode has ANSI_QUOTES, \
                 and kilit does not know the server's sql_mode",
                "{sql}"
            );
        }
    }

    #[test]
    fn double_quotes_where_mysql_expects_a_string_keep_the_statement() {
        for sql in [
            r#"ALTER TABLE orders ADD COLUMN c_default VARCHAR(10) NOT NULL DEFAULT "x""#,
            r#"ALTER TABLE orders ADD COLUMN c_collate VARCHAR(10) COLLATE "utf8mb4_bin" NULL"#,
            r#"ALTER TABLE orders ADD COLUMN c_charset VARCHAR(10) CHARACTER SET "utf8mb4" NULL"#,
            r#"ALTER TABLE orders ADD COLUMN c_enum ENUM("a", "b") NULL"#,
            r#"ALTER TABLE d ALTER COLUMN note SET DEFAULT "x""#,
            "ALTER TABLE orders ADD COLUMN `coupon` INT NULL",
            "ALTER TABLE orders ADD INDEX `ix_amount` (`amount`)",
        ] {
            assert!(matches!(kind(sql), StatementKind::Alter(_)), "{sql}");
        }
        assert_eq!(
            added_column(
                r#"ALTER TABLE orders ADD COLUMN c_collate VARCHAR(10) COLLATE "utf8mb4_bin" NULL"#
            )
            .collation
            .as_deref(),
            Some("utf8mb4_bin")
        );
    }

    #[test]
    fn using_between_the_table_and_the_columns_gets_no_answer() {
        for sql in [
            "CREATE INDEX i_using_1 ON orders USING BTREE (amount)",
            "CREATE INDEX i_using_2 ON orders USING HASH (amount)",
            "CREATE UNIQUE INDEX u_using_3 ON orders USING BTREE (email)",
            "CREATE INDEX i_using_4 ON k5bmanual.orders USING BTREE (amount)",
            "CREATE INDEX i_using_5 ON orders USING BTREE (amount) USING BTREE",
        ] {
            assert_eq!(
                no_answer_for(sql),
                "MySQL 8.0.46 and 8.4.13 do not accept USING between the table name and \
                 the column list in CREATE INDEX",
                "{sql}"
            );
        }
        // Before ON and after the column list, MySQL takes it.
        for sql in [
            "CREATE INDEX i_using_6 USING BTREE ON orders (amount)",
            "CREATE INDEX i_using_7 ON orders (amount) USING BTREE",
            "CREATE INDEX i_using_8 USING BTREE ON orders (amount) USING HASH",
            "ALTER TABLE orders ADD INDEX i_using_9 USING BTREE (amount)",
            "ALTER TABLE orders ADD INDEX i_using_10 (amount) USING BTREE",
        ] {
            assert!(matches!(kind(sql), StatementKind::Alter(_)), "{sql}");
        }
    }

    #[test]
    fn defaults_mysql_takes_without_parentheses_keep_the_statement() {
        // Literals, and CURRENT_TIMESTAMP and its synonyms in a column
        // definition. In parentheses, a default is an expression, which
        // `check` answers.
        for default in [
            "0",
            "-1",
            "+1",
            "- 1",
            "1.5",
            "-1.5",
            ".5",
            "-.5",
            "1.",
            "1e3",
            "-1e3",
            "'x'",
            "''",
            "'a b'",
            "'it''s'",
            r#""x""#,
            "N'x'",
            "_utf8mb4'x'",
            "_utf8mb4 0x41",
            r#"_utf8mb4"x""#,
            "'a' 'b'",
            "'x' COLLATE utf8mb4_bin",
            "NULL",
            "null",
            "TRUE",
            "false",
            "b'01'",
            "B'01'",
            "0b01",
            "x'0A'",
            "X'0A'",
            "0x0A",
            "_binary x'0A'",
            "CURRENT_TIMESTAMP",
            "current_timestamp",
            "CURRENT_TIMESTAMP(6)",
            "CURRENT_TIMESTAMP ( 6 )",
            "CURRENT_TIMESTAMP()",
            "CURRENT_TIMESTAMP(0)",
            "CURRENT_TIMESTAMP(06)",
            "CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP",
            "CURRENT_TIMESTAMP(6) ON UPDATE CURRENT_TIMESTAMP(6)",
            "NOW()",
            "now(6)",
            "LOCALTIME",
            "LOCALTIME()",
            "LOCALTIME(6)",
            "LOCALTIMESTAMP",
            "LOCALTIMESTAMP()",
            "LOCALTIMESTAMP ( )",
            "LOCALTIMESTAMP(6)",
            "(CURRENT_DATE)",
            "(DATE '2020-01-01')",
        ] {
            for sql in [
                format!("ALTER TABLE d ADD COLUMN c INT NULL DEFAULT {default}"),
                format!("ALTER TABLE d MODIFY c INT NULL DEFAULT {default}"),
                format!("ALTER TABLE d CHANGE c c INT NULL DEFAULT {default}"),
            ] {
                assert!(matches!(kind(&sql), StatementKind::Alter(_)), "{sql}");
            }
        }
        // ALTER COLUMN ... SET DEFAULT takes literals.
        for default in [
            "0",
            "-1",
            "+1",
            "1.5",
            "1e3",
            "'x'",
            "''",
            r#""x""#,
            "N'x'",
            "_utf8mb4'x'",
            "'a' 'b'",
            "NULL",
            "TRUE",
            "FALSE",
            "b'01'",
            "0b01",
            "x'0A'",
            "0x0A",
            "_binary x'0A'",
            "(NOW())",
        ] {
            let sql = format!("ALTER TABLE d ALTER COLUMN c SET DEFAULT {default}");
            assert!(matches!(kind(&sql), StatementKind::Alter(_)), "{sql}");
        }
    }

    #[test]
    fn other_defaults_without_parentheses_get_no_answer() {
        // Error 1064, or 1067 for the date and time literals. The reason has
        // the default as the parser prints it.
        for (default, printed) in [
            ("+-1", "+-1"),
            ("-'1'", "-'1'"),
            ("-0x0A", "-X'0A'"),
            ("0X0A", "0X0A"),
            ("0B01", "0B01"),
            (r#"b"01""#, r#"B"01""#),
            ("UNKNOWN", "UNKNOWN"),
            ("DATE '2020-01-01'", "DATE '2020-01-01'"),
            ("TIME '10:00:00'", "TIME '10:00:00'"),
            (
                "TIMESTAMP '2020-01-01 00:00:00'",
                "TIMESTAMP '2020-01-01 00:00:00'",
            ),
            (
                "DATETIME '2020-01-01 00:00:00'",
                "DATETIME '2020-01-01 00:00:00'",
            ),
            ("{d '2020-01-01'}", "{d '2020-01-01'}"),
            ("NOW", "NOW"),
            ("NOW(+6)", "NOW(+6)"),
            ("CURRENT_TIMESTAMP(6.0)", "CURRENT_TIMESTAMP(6.0)"),
            ("`NOW`()", "`NOW`()"),
            ("CURRENT_DATE", "CURRENT_DATE"),
            ("CURRENT_DATE()", "CURRENT_DATE()"),
            ("CURDATE()", "CURDATE()"),
            ("CURRENT_TIME", "CURRENT_TIME"),
            ("SYSDATE()", "SYSDATE()"),
            ("UTC_TIMESTAMP", "UTC_TIMESTAMP"),
            ("UTC_TIMESTAMP()", "UTC_TIMESTAMP()"),
            (
                "CURRENT_TIMESTAMP + INTERVAL 1 DAY",
                "CURRENT_TIMESTAMP + INTERVAL 1 DAY",
            ),
            ("1+1", "1 + 1"),
            ("UUID()", "UUID()"),
            ("abc", "abc"),
            ("CURRENT_USER", "CURRENT_USER"),
            ("@x", "@x"),
        ] {
            for sql in [
                format!("ALTER TABLE d ADD COLUMN c DATE NULL DEFAULT {default}"),
                format!("ALTER TABLE d MODIFY day DATE NULL DEFAULT {default}"),
                format!("ALTER TABLE d CHANGE day day DATE NULL DEFAULT {default}"),
            ] {
                assert_eq!(
                    no_answer_for(&sql),
                    format!(
                        "MySQL 8.0.46 and 8.4.13 do not accept DEFAULT {printed} \
                         without parentheses"
                    ),
                    "{sql}"
                );
            }
        }
        // ALTER COLUMN ... SET DEFAULT takes no CURRENT_TIMESTAMP.
        for (default, printed) in [
            ("CURRENT_TIMESTAMP", "CURRENT_TIMESTAMP"),
            ("CURRENT_TIMESTAMP()", "CURRENT_TIMESTAMP()"),
            ("CURRENT_TIMESTAMP(6)", "CURRENT_TIMESTAMP(6)"),
            ("NOW()", "NOW()"),
            ("LOCALTIME", "LOCALTIME"),
            ("LOCALTIMESTAMP", "LOCALTIMESTAMP"),
            ("CURRENT_DATE", "CURRENT_DATE"),
            ("'x' COLLATE utf8mb4_bin", "'x' COLLATE utf8mb4_bin"),
            ("1+1", "1 + 1"),
            ("CURRENT_TIMESTAMP + 0", "CURRENT_TIMESTAMP + 0"),
            ("DATE '2020-01-01'", "DATE '2020-01-01'"),
            (
                "TIMESTAMP '2020-01-01 00:00:00'",
                "TIMESTAMP '2020-01-01 00:00:00'",
            ),
        ] {
            let sql = format!("ALTER TABLE d ALTER COLUMN seen SET DEFAULT {default}");
            assert_eq!(
                no_answer_for(&sql),
                format!(
                    "MySQL 8.0.46 and 8.4.13 do not accept SET DEFAULT {printed} \
                     without parentheses"
                ),
                "{sql}"
            );
        }
    }

    #[test]
    fn a_key_part_that_is_not_a_prefix_gets_no_answer() {
        for (sql, part) in [
            ("ALTER TABLE k ADD INDEX k9 (lower(email))", "lower(email)"),
            (
                "ALTER TABLE k ADD UNIQUE INDEX k10 (lower(email))",
                "lower(email)",
            ),
            (
                "ALTER TABLE k ADD FULLTEXT INDEX k11 (lower(note))",
                "lower(note)",
            ),
            ("CREATE INDEX k12 ON k (lower(email))", "lower(email)"),
            (
                "CREATE UNIQUE INDEX k13 ON k (lower(email))",
                "lower(email)",
            ),
            // $table->rawIndex('lower(email)', 'k_lower_email_index')
            (
                "alter table `k` add index `k_lower_email_index`(lower(email))",
                "lower(email)",
            ),
            ("ALTER TABLE k ADD INDEX k14 (note(10, 2))", "note(10, 2)"),
            ("ALTER TABLE k ADD INDEX k15 (note(10.5))", "note(10.5)"),
            ("ALTER TABLE k ADD INDEX k16 (note(1e1))", "note(1e1)"),
            ("ALTER TABLE k ADD INDEX k17 (note(0x0A))", "note(X'0A')"),
            ("ALTER TABLE k ADD INDEX k18 (note(+10))", "note(+10)"),
            ("ALTER TABLE k ADD INDEX k19 (note())", "note()"),
            ("ALTER TABLE k ADD INDEX k20 (k.note(10))", "k.note(10)"),
            (
                "ALTER TABLE k2 ADD PRIMARY KEY (lower(email))",
                "lower(email)",
            ),
        ] {
            assert_eq!(
                no_answer_for(sql),
                format!(
                    "MySQL 8.0.46 and 8.4.13 do not accept the key part {part}; an expression \
                     in an index needs its own parentheses, and a prefix length is a whole \
                     number"
                ),
                "{sql}"
            );
        }
        // A prefix, and an expression in its own parentheses.
        for sql in [
            "ALTER TABLE k ADD INDEX k1 (note(10))",
            "ALTER TABLE k ADD INDEX k2 (`note`(10))",
            "ALTER TABLE k ADD INDEX k3 (NOTE(10))",
            "ALTER TABLE k ADD INDEX k4 (note (10))",
            "ALTER TABLE k ADD INDEX k5 (note(010))",
            "ALTER TABLE k ADD INDEX k6 (note(10) DESC)",
            "ALTER TABLE k ADD INDEX k7 (`note`(10), email)",
            "ALTER TABLE k ADD INDEX k8 ((lower(email)))",
            "ALTER TABLE k2 ADD PRIMARY KEY (`note`(10))",
        ] {
            assert!(matches!(kind(sql), StatementKind::Alter(_)), "{sql}");
        }
    }

    #[test]
    fn truncate_and_lock_tables_get_no_answer() {
        for sql in ["TRUNCATE TABLE orders", "TRUNCATE orders"] {
            assert_eq!(
                no_answer_for(sql),
                "truncating a table has not been measured yet; TRUNCATE removes every row",
                "{sql}"
            );
        }
        assert_eq!(
            no_answer_for("LOCK TABLES orders WRITE, users READ"),
            "LOCK TABLES has not been measured yet; other connections cannot write \
             to the tables until UNLOCK TABLES"
        );
    }

    #[test]
    fn the_same_statements_in_mysql_s_words_keep_their_kind() {
        // Next to each word above, what MySQL accepts.
        for sql in [
            "ALTER TABLE orders ADD COLUMN coupon INT NULL",
            "ALTER TABLE orders DROP COLUMN note",
            "ALTER TABLE orders ADD CONSTRAINT fk FOREIGN KEY (user_id) REFERENCES users (id)",
            "CREATE INDEX i ON orders (amount)",
            "CREATE UNIQUE INDEX i ON orders (amount)",
            "CREATE INDEX i USING BTREE ON orders (amount)",
            "CREATE INDEX i ON orders (amount) USING BTREE",
            "CREATE INDEX i ON orders (amount DESC)",
            "CREATE INDEX i ON orders (amount) ALGORITHM=INPLACE LOCK=NONE",
            "DROP INDEX orders_status_index ON orders",
            "ALTER TABLE orders DROP INDEX orders_status_index",
            "ALTER TABLE orders ADD COLUMN coupon INT NULL, ALGORITHM=INSTANT",
            "ALTER TABLE `orders` ADD COLUMN `coupon` INT NULL",
            "alter table `orders` drop index `orders_status_index`",
            "alter table `orders` add constraint `orders_user_id_foreign` \
             foreign key (`user_id`) references `users` (`id`)",
        ] {
            assert!(matches!(kind(sql), StatementKind::Alter(_)), "{sql}");
        }
        assert_eq!(
            alter("CREATE INDEX i ON orders (amount) ALGORITHM=INPLACE LOCK=NONE"),
            Alter {
                table: "orders".into(),
                operations: vec![Operation::AddIndex(Index {
                    name: Some("i".into()),
                    kind: IndexKind::Plain,
                    columns: vec!["amount".into()],
                })],
                algorithm: Some(Algorithm::Inplace),
                lock: Some(Lock::None),
            }
        );
        assert_eq!(
            kind("DROP TABLE IF EXISTS shops"),
            StatementKind::DropTable {
                tables: vec!["shops".into()]
            }
        );
        assert!(matches!(
            kind("CREATE TABLE IF NOT EXISTS drafts (id INT PRIMARY KEY)"),
            StatementKind::CreateTable {
                if_not_exists: true,
                ..
            }
        ));
        assert!(matches!(
            kind("RENAME TABLE shops TO stores"),
            StatementKind::RenameTable { .. }
        ));
        for sql in [
            "INSERT INTO shops (name) VALUES ('a')",
            "UPDATE orders SET amount = 1",
            "DELETE FROM orders",
            "SELECT * FROM orders",
            "UNLOCK TABLES",
        ] {
            assert_eq!(kind(sql), StatementKind::Other, "{sql}");
        }
        assert!(matches!(
            kind("SET foreign_key_checks = 0"),
            StatementKind::Set { .. }
        ));
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
            // An index statement names its table after ON. MySQL 8.0.46 and
            // 8.4.13 accept each one, run by hand.
            (
                "CREATE FULLTEXT INDEX ft_note ON orders (note)",
                Some("orders"),
            ),
            (
                "CREATE FULLTEXT INDEX ft_email ON `shop`.`orders` (email) WITH PARSER ngram",
                Some("shop.orders"),
            ),
            (
                "CREATE SPATIAL INDEX sp_spot ON places (spot)",
                Some("places"),
            ),
            (
                "CREATE INDEX i_invisible ON orders (amount) INVISIBLE",
                Some("orders"),
            ),
            (
                "CREATE UNIQUE INDEX u_invisible ON orders (email) INVISIBLE",
                Some("orders"),
            ),
            (
                "CREATE INDEX i_block ON orders (amount) KEY_BLOCK_SIZE=8",
                Some("orders"),
            ),
            (
                "DROP INDEX i_drop_1 ON orders ALGORITHM=INPLACE",
                Some("orders"),
            ),
            ("DROP INDEX i_drop_2 ON orders LOCK=NONE", Some("orders")),
            (
                "DROP INDEX `on` ON orders ALGORITHM=INPLACE LOCK=NONE",
                Some("orders"),
            ),
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
