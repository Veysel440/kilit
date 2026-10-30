//! Prints what kilit-core reads from a SQL file, statement by statement.
//!
//!     cargo run -p kilit-core --example parse -- path/to/migration.sql
//!
//! Without a path it reads `examples/sample.sql` next to this file.

use std::process::ExitCode;

use kilit_core::StatementKind;

fn main() -> ExitCode {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/examples/sample.sql").to_owned());
    let sql = match std::fs::read_to_string(&path) {
        Ok(sql) => sql,
        Err(err) => {
            eprintln!("error: {path}: {err}");
            return ExitCode::FAILURE;
        }
    };

    for statement in kilit_core::parse(&sql) {
        println!("line {}: {}", statement.line, statement.sql);
        match statement.kind {
            StatementKind::Alter(alter) => {
                let mut forced = Vec::new();
                if let Some(algorithm) = alter.algorithm {
                    forced.push(format!("ALGORITHM={algorithm}"));
                }
                if let Some(lock) = alter.lock {
                    forced.push(format!("LOCK={lock}"));
                }
                if forced.is_empty() {
                    println!("  alter table {}", alter.table);
                } else {
                    println!("  alter table {} ({})", alter.table, forced.join(", "));
                }
                for operation in &alter.operations {
                    println!("    {operation}");
                }
            }
            StatementKind::CreateTable {
                table,
                if_not_exists,
            } => {
                let condition = if if_not_exists { " if not exists" } else { "" };
                println!("  create table{condition} {}", table.name);
                for column in &table.columns {
                    println!("    column {column}");
                }
                if !table.primary_key.is_empty() {
                    println!("    primary key ({})", table.primary_key.join(", "));
                }
                for index in &table.indexes {
                    println!("    {index}");
                }
                for key in &table.foreign_keys {
                    println!("    {key}");
                }
                if let Some(charset) = &table.charset {
                    println!("    default character set {charset}");
                }
                if let Some(collation) = &table.collation {
                    println!("    default collation {collation}");
                }
            }
            StatementKind::DropTable { tables } => println!("  drop table {}", tables.join(", ")),
            StatementKind::RenameTable { renames } => {
                for rename in renames {
                    println!("  rename table {} to {}", rename.from, rename.to);
                }
            }
            StatementKind::Set { variables } => {
                for variable in variables {
                    println!("  set {} = {}", variable.name, variable.value);
                }
            }
            StatementKind::Other => println!("  not analyzed"),
            StatementKind::Unparsed { error, table } => match table {
                Some(table) => println!("  NOT READ, on {table}: {error}"),
                None => println!("  NOT READ: {error}"),
            },
        }
        println!();
    }
    ExitCode::SUCCESS
}
