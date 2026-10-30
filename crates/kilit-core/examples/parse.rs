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
            StatementKind::CreateTable { table } => println!("  create table {table}"),
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
            StatementKind::Unparsed { error } => println!("  NOT READ: {error}"),
        }
        println!();
    }
    ExitCode::SUCCESS
}
