//! What kilit checks: SQL files, saved `migrate --pretend` output, or either
//! of them piped into standard input.

use std::io::Read;
use std::path::{Path, PathBuf};

use kilit_core::{LaravelError, Statement, UnreadableLine, parse, read_pretend};
use serde::Serialize;

/// Everything to check, in the order it runs.
pub struct Input {
    pub sources: Vec<Source>,
    /// Lines of Laravel's output that kilit does not understand.
    pub unreadable: Vec<Unreadable>,
    /// Some input was the output of `migrate --pretend`.
    pub from_laravel: bool,
    /// Each time Laravel stopped with an error.
    pub laravel_errors: Vec<LaravelStop>,
    /// Some input holds a `/*! ... */` comment, which MySQL runs. kilit does
    /// not see a statement made of one alone.
    pub runs_comments: bool,
    /// How many inputs were read: files, or standard input.
    sessions: usize,
}

/// Statements that belong together: one Laravel migration, or one SQL file.
pub struct Source {
    /// The migration's name, the file's path, or `standard input`.
    pub name: String,
    pub kind: SourceKind,
    pub statements: Vec<Statement>,
    /// Which of `statements` is the first that asks the database something,
    /// in a Laravel migration.
    pub first_read: Option<usize>,
    /// The input it comes from, counting from 0: each SQL file runs in a
    /// session of its own, and so does each `migrate --pretend` output, as
    /// one `artisan migrate` uses one connection.
    pub session: usize,
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

/// Laravel's error screen in one input.
pub struct LaravelStop {
    /// The file it is in, or `standard input`.
    pub input: String,
    pub error: LaravelError,
}

const STDIN: &str = "standard input";

impl Input {
    /// Reads `files` in order; `-` is standard input.
    pub fn read(files: &[PathBuf]) -> Result<Input, String> {
        let mut input = Input {
            sources: Vec::new(),
            unreadable: Vec::new(),
            from_laravel: false,
            laravel_errors: Vec::new(),
            runs_comments: false,
            sessions: 0,
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
        let session = self.sessions;
        self.sessions += 1;
        self.runs_comments |= text.contains("/*!");
        match read_pretend(text) {
            Some(pretend) => {
                self.from_laravel = true;
                for migration in pretend.migrations {
                    self.sources.push(Source {
                        name: migration.name,
                        kind: SourceKind::LaravelMigration,
                        statements: migration.statements,
                        first_read: migration.first_read,
                        session,
                    });
                }
                for line in pretend.unreadable {
                    self.unreadable.push(Unreadable {
                        input: name.clone(),
                        line,
                    });
                }
                if let Some(error) = pretend.error {
                    self.laravel_errors.push(LaravelStop { input: name, error });
                }
            }
            None => self.sources.push(Source {
                name,
                kind: SourceKind::Sql,
                statements: parse(text),
                first_read: None,
                session,
            }),
        }
    }
}

impl Input {
    /// How many inputs were read: files, or standard input.
    pub fn inputs(&self) -> usize {
        self.sessions
    }
}

/// The server versions a dump names: the text after `-- Server version`
/// and a tab, as mysqldump and mariadb-dump write it, or spaces, each one
/// once, in order. A dump made of several has several. The `Distrib` line
/// names the version of mysqldump itself, not of the server, and is not
/// read.
pub fn server_versions(text: &str) -> Vec<&str> {
    let mut versions: Vec<&str> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("-- Server version") else {
            continue;
        };
        if !rest.starts_with([' ', '\t']) {
            continue;
        }
        let version = rest.trim();
        if !version.is_empty() && !versions.contains(&version) {
            versions.push(version);
        }
    }
    versions
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
    fn reads_the_server_version_a_dump_names() {
        // mysqldump 8.4.13 run against the 8.0.46 server: line 1 names the
        // client, line 5 the server.
        let dump = "-- MySQL dump 10.13  Distrib 8.4.13, for Linux (x86_64)\n\
                    --\n\
                    -- Host: localhost    Database: app\n\
                    -- ------------------------------------------------------\n\
                    -- Server version\t8.0.46\n\
                    \n\
                    CREATE TABLE t (id int);\n";
        assert_eq!(server_versions(dump), ["8.0.46"]);
        assert_eq!(
            server_versions("-- Server version   8.4.13  \r\n"),
            ["8.4.13"]
        );
    }

    #[test]
    fn reads_every_server_version_of_a_joined_dump_once() {
        let dump = "-- Server version\t8.0.46\nCREATE TABLE a (id int);\n\
                    -- Server version\t10.11.6-MariaDB-0ubuntu0.23.04.1\n\
                    -- Server version\t8.0.46\n";
        assert_eq!(
            server_versions(dump),
            ["8.0.46", "10.11.6-MariaDB-0ubuntu0.23.04.1"]
        );
    }

    #[test]
    fn reads_no_server_version_from_other_lines() {
        for dump in [
            // What Laravel's schema:dump writes: no comments at all.
            "/*!40103 SET TIME_ZONE='+00:00' */;\nCREATE TABLE t (id int);\n",
            "-- MySQL dump 10.13  Distrib 8.0.46, for Linux (x86_64)\n",
            "-- Server version\n",
            "-- Server version\t\n",
            "-- Server versions\t8.0.46\n",
            " -- Server version\t8.0.46\n",
            "--  Server version\t8.0.46\n",
            "CREATE TABLE t (id int) COMMENT '-- Server version 8.0.46';\n",
        ] {
            assert_eq!(server_versions(dump), Vec::<&str>::new(), "{dump:?}");
        }
    }

    #[test]
    fn refuses_other_bytes() {
        assert_eq!(
            decode(b"\xFF\x00\xFE", "dump.bin").unwrap_err(),
            "dump.bin: not UTF-8 or UTF-16 text"
        );
    }
}
