//! What the lab runs: scenarios read from `scenarios/*.toml`, and the
//! variants each one is tried with.

use std::fmt;
use std::fs;
use std::path::Path;

use kilit_core::Algorithm;
use serde::{Deserialize, Serialize};

use crate::Result;

/// One schema change to measure, read from `scenarios/<id>.toml`.
pub struct Scenario {
    /// The file name without `.toml`, e.g. `add-index`. Two files in one
    /// directory cannot share a name, so ids are unique by construction.
    pub id: String,
    pub title: String,
    /// Statements run in the same session right before the ALTER,
    /// e.g. `SET foreign_key_checks = 0`.
    pub session: Vec<String>,
    /// Everything that follows `ALTER TABLE orders`.
    pub operation: String,
}

/// The contents of a scenario file. Unknown keys are an error, so a typo
/// such as `operaton` cannot silently drop a field.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioFile {
    title: String,
    #[serde(default)]
    session: Vec<String>,
    operation: String,
}

impl Scenario {
    /// The ALTER statement for one variant.
    pub fn statement(&self, variant: Variant) -> String {
        format!("ALTER TABLE orders {}{}", self.operation, variant.clause())
    }
}

/// Reads every `*.toml` file in `dir`, sorted by id.
pub fn load_all(dir: &Path) -> Result<Vec<Scenario>> {
    let entries = fs::read_dir(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let mut scenarios = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            let scenario = load(&path).map_err(|err| format!("{}: {err}", path.display()))?;
            scenarios.push(scenario);
        }
    }
    if scenarios.is_empty() {
        return Err(format!("no scenario files in {}", dir.display()).into());
    }
    scenarios.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(scenarios)
}

fn load(path: &Path) -> Result<Scenario> {
    let id = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("the file name is not valid UTF-8")?;
    parse(id, &fs::read_to_string(path)?)
}

fn parse(id: &str, text: &str) -> Result<Scenario> {
    check_id(id)?;
    let file: ScenarioFile = toml::from_str(text)?;
    Ok(Scenario {
        id: id.to_owned(),
        title: file.title,
        session: file.session,
        operation: file.operation,
    })
}

/// Ids appear in reports and result files: lowercase letters, digits and
/// single hyphens, like `add-unique-index`.
fn check_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("--")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "`{id}` is not a valid scenario id: use lowercase letters, digits and single hyphens"
        )
        .into())
    }
}

/// One way of running a scenario's ALTER statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Variant {
    /// `None` lets the server choose, which is what Laravel and most tools do.
    pub algorithm: Option<Algorithm>,
    /// Append `LOCK=NONE`: the server must keep the table writable or refuse.
    pub lock_none: bool,
}

impl Variant {
    /// Nothing forced: what a migration normally sends.
    pub const SERVER_DEFAULT: Variant = Variant {
        algorithm: None,
        lock_none: false,
    };

    /// Only `LOCK=NONE`: can the server do this while the table stays writable?
    pub const LOCK_NONE: Variant = Variant {
        algorithm: None,
        lock_none: true,
    };

    /// Only `ALGORITHM=...`.
    pub const fn forced(algorithm: Algorithm) -> Variant {
        Variant {
            algorithm: Some(algorithm),
            lock_none: false,
        }
    }

    /// Every variant the lab tries, in report order.
    pub const ALL: [Variant; 6] = [
        Variant::SERVER_DEFAULT,
        Variant::forced(Algorithm::Instant),
        Variant::forced(Algorithm::Inplace),
        Variant {
            algorithm: Some(Algorithm::Inplace),
            lock_none: true,
        },
        Variant::forced(Algorithm::Copy),
        Variant::LOCK_NONE,
    ];

    /// The options this variant adds, e.g. `["ALGORITHM=INPLACE", "LOCK=NONE"]`.
    fn options(self) -> Vec<String> {
        let mut options = Vec::new();
        if let Some(algorithm) = self.algorithm {
            options.push(format!("ALGORITHM={algorithm}"));
        }
        if self.lock_none {
            options.push("LOCK=NONE".to_owned());
        }
        options
    }

    /// The text appended to the ALTER statement, e.g. `, ALGORITHM=INPLACE, LOCK=NONE`.
    pub fn clause(self) -> String {
        self.options()
            .iter()
            .map(|option| format!(", {option}"))
            .collect()
    }
}

impl fmt::Display for Variant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let options = self.options();
        if options.is_empty() {
            f.write_str("(server default)")
        } else {
            f.write_str(&options.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_default_adds_nothing() {
        assert_eq!(Variant::SERVER_DEFAULT.clause(), "");
        assert_eq!(Variant::SERVER_DEFAULT.to_string(), "(server default)");
    }

    #[test]
    fn algorithm_comes_before_lock() {
        let variant = Variant {
            algorithm: Some(Algorithm::Inplace),
            lock_none: true,
        };
        assert_eq!(variant.clause(), ", ALGORITHM=INPLACE, LOCK=NONE");
        assert_eq!(variant.to_string(), "ALGORITHM=INPLACE, LOCK=NONE");
    }

    #[test]
    fn lock_can_be_asked_without_an_algorithm() {
        assert_eq!(Variant::LOCK_NONE.clause(), ", LOCK=NONE");
    }

    #[test]
    fn every_variant_is_tried_once() {
        for (i, a) in Variant::ALL.iter().enumerate() {
            for b in &Variant::ALL[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn statement_appends_the_variant() {
        let scenario = parse(
            "add-index",
            r#"title = "t"
operation = "ADD INDEX i (amount)""#,
        )
        .unwrap();
        assert_eq!(
            scenario.statement(Variant::forced(Algorithm::Copy)),
            "ALTER TABLE orders ADD INDEX i (amount), ALGORITHM=COPY"
        );
    }

    #[test]
    fn session_is_optional() {
        let scenario = parse("x", "title = \"t\"\noperation = \"ADD INDEX i (amount)\"").unwrap();
        assert!(scenario.session.is_empty());

        let scenario = parse(
            "x",
            "title = \"t\"\nsession = [\"SET foreign_key_checks = 0\"]\noperation = \"o\"",
        )
        .unwrap();
        assert_eq!(scenario.session, ["SET foreign_key_checks = 0"]);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let typo = parse("x", "title = \"t\"\noperaton = \"ADD INDEX i (amount)\"");
        assert!(typo.is_err());
    }

    #[test]
    fn ids_are_lowercase_words_joined_by_hyphens() {
        for good in ["add-index", "varchar-50-to-60", "x"] {
            assert!(check_id(good).is_ok(), "{good} should be valid");
        }
        for bad in [
            "",
            "Add-Index",
            "add_index",
            "-add",
            "add-",
            "add--index",
            "add index",
        ] {
            assert!(check_id(bad).is_err(), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn every_scenario_file_in_the_repository_loads() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scenarios");
        let scenarios = load_all(&dir).unwrap();
        assert!(scenarios.len() >= 5);
    }

    /// kilit can only use a measurement if it can read the statement that
    /// was measured: every statement the lab sends must come back from
    /// kilit-core as exactly what the lab meant.
    #[test]
    fn kilit_core_reads_every_statement_the_lab_runs() {
        use kilit_core::{Lock, Operation, Statement, StatementKind};

        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scenarios");
        for scenario in load_all(&dir).unwrap() {
            for sql in &scenario.session {
                let statements = kilit_core::parse(sql);
                assert!(
                    matches!(
                        statements.as_slice(),
                        [Statement {
                            kind: StatementKind::Set { .. },
                            ..
                        }]
                    ),
                    "{}: {statements:?}",
                    scenario.id
                );
            }

            for variant in Variant::ALL {
                let sql = scenario.statement(variant);
                let statements = kilit_core::parse(&sql);
                let [
                    Statement {
                        kind: StatementKind::Alter(alter),
                        ..
                    },
                ] = statements.as_slice()
                else {
                    panic!("{}: not one ALTER TABLE: {statements:?}", scenario.id);
                };
                assert_eq!(alter.table, "orders", "{sql}");
                assert_eq!(alter.algorithm, variant.algorithm, "{sql}");
                assert_eq!(alter.lock, variant.lock_none.then_some(Lock::None), "{sql}");
                assert!(!alter.operations.is_empty(), "{sql}");
                for operation in &alter.operations {
                    // An `Other` operation prints as "not understood: ...".
                    assert!(
                        !matches!(operation, Operation::Other { .. }),
                        "{}: {operation}",
                        scenario.id
                    );
                }
            }
        }
    }
}
