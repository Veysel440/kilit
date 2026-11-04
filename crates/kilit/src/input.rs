//! What kilit checks: SQL files, saved `migrate --pretend` output, or either
//! of them piped into standard input.

use std::io::Read;
use std::path::{Path, PathBuf};

use kilit_core::{Statement, UnreadableLine, parse, read_pretend};
use serde::Serialize;

/// Everything to check, in the order it runs.
pub struct Input {
    pub sources: Vec<Source>,
    /// Lines of Laravel's output that kilit does not understand.
    pub unreadable: Vec<Unreadable>,
    /// Some input was the output of `migrate --pretend`.
    pub from_laravel: bool,
}

/// Statements that belong together: one Laravel migration, or one SQL file.
pub struct Source {
    /// The migration's name, the file's path, or `standard input`.
    pub name: String,
    pub kind: SourceKind,
    pub statements: Vec<Statement>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// From `migrate --pretend`; its statements' lines are lines of that
    /// output.
    LaravelMigration,
    Sql,
}

pub struct Unreadable {
    /// The file it is in, or `standard input`.
    pub input: String,
    pub line: UnreadableLine,
}

const STDIN: &str = "standard input";

impl Input {
    /// Reads `files` in order; `-` is standard input.
    pub fn read(files: &[PathBuf]) -> Result<Input, String> {
        let mut input = Input {
            sources: Vec::new(),
            unreadable: Vec::new(),
            from_laravel: false,
        };
        let mut stdin_read = false;
        for file in files {
            if file.as_os_str() == "-" {
                if stdin_read {
                    return Err("standard input (`-`) can be read only once".into());
                }
                stdin_read = true;
                let mut bytes = Vec::new();
                std::io::stdin()
                    .read_to_end(&mut bytes)
                    .map_err(|err| format!("{STDIN}: {err}"))?;
                input.add(STDIN.to_owned(), &decode(&bytes, STDIN)?);
            } else {
                input.add(file.display().to_string(), &read(file)?);
            }
        }
        Ok(input)
    }

    fn add(&mut self, name: String, text: &str) {
        match read_pretend(text) {
            Some(pretend) => {
                self.from_laravel = true;
                for migration in pretend.migrations {
                    self.sources.push(Source {
                        name: migration.name,
                        kind: SourceKind::LaravelMigration,
                        statements: migration.statements,
                    });
                }
                for line in pretend.unreadable {
                    self.unreadable.push(Unreadable {
                        input: name.clone(),
                        line,
                    });
                }
            }
            None => self.sources.push(Source {
                name,
                kind: SourceKind::Sql,
                statements: parse(text),
            }),
        }
    }
}

/// The text of a file.
pub fn read(path: &Path) -> Result<String, String> {
    let name = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|err| format!("{name}: {err}"))?;
    decode(&bytes, &name)
}

/// Text from bytes: UTF-8, with or without a byte order mark, or UTF-16 with
/// one, which Windows PowerShell 5 writes for `... > pretend.txt`.
fn decode(bytes: &[u8], name: &str) -> Result<String, String> {
    let text = if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8(rest.to_vec()).ok()
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        utf16(rest, u16::from_le_bytes)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        utf16(rest, u16::from_be_bytes)
    } else {
        String::from_utf8(bytes.to_vec()).ok()
    };
    text.ok_or_else(|| format!("{name}: not UTF-8 or UTF-16 text"))
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> Option<String> {
    let (pairs, rest) = bytes.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    char::decode_utf16(pairs.iter().map(|&pair| unit(pair)))
        .collect::<Result<String, _>>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_utf8_with_or_without_a_byte_order_mark() {
        assert_eq!(decode(b"select 1", "x").unwrap(), "select 1");
        assert_eq!(decode(b"\xEF\xBB\xBFselect 1", "x").unwrap(), "select 1");
    }

    #[test]
    fn reads_what_windows_powershell_writes() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "⇂ alter".encode_utf16() {
            bytes.extend(unit.to_le_bytes());
        }
        assert_eq!(decode(&bytes, "x").unwrap(), "⇂ alter");
    }

    #[test]
    fn refuses_other_bytes() {
        assert_eq!(
            decode(b"\xFF\x00\xFE", "dump.bin").unwrap_err(),
            "dump.bin: not UTF-8 or UTF-16 text"
        );
    }
}
