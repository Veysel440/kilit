//! The character sets kilit knows, for the byte arithmetic of text columns.
//!
//! MySQL stores a VARCHAR's length in 1 byte up to 255 bytes, and in 2
//! bytes beyond. Whether a longer VARCHAR crosses that line depends on how
//! many bytes a character can take, so kilit needs these numbers.

/// A character set as MySQL 8.0 and 8.4 define it. kilit-lab checks every
/// entry of [`CHARSETS`] against each server it measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Charset {
    pub name: &'static str,
    /// The most bytes one character can take.
    pub max_bytes: u64,
    /// The collation a column gets when it names only the character set.
    pub default_collation: &'static str,
}

pub const CHARSETS: [Charset; 5] = [
    Charset {
        name: "ascii",
        max_bytes: 1,
        default_collation: "ascii_general_ci",
    },
    Charset {
        name: "binary",
        max_bytes: 1,
        default_collation: "binary",
    },
    Charset {
        name: "latin1",
        max_bytes: 1,
        default_collation: "latin1_swedish_ci",
    },
    Charset {
        name: "utf8mb3",
        max_bytes: 3,
        default_collation: "utf8mb3_general_ci",
    },
    Charset {
        name: "utf8mb4",
        max_bytes: 4,
        default_collation: "utf8mb4_0900_ai_ci",
    },
];

impl Charset {
    /// A character set by name, in any case and with or without quotes.
    pub fn named(name: &str) -> Option<&'static Charset> {
        let name = canonical(name);
        CHARSETS.iter().find(|charset| charset.name == name)
    }

    /// The character set a collation belongs to: its name up to the first
    /// underscore, as in `utf8mb4_unicode_ci`.
    pub fn of_collation(collation: &str) -> Option<&'static Charset> {
        let collation = canonical(collation);
        let name = collation.split('_').next().unwrap_or_default();
        Charset::named(name)
    }
}

/// A character set or collation name as MySQL 8 prints it: lowercase,
/// without quotes, and with the old `utf8` spelled `utf8mb3`.
pub(crate) fn canonical(name: &str) -> String {
    let name = name.trim_matches(['\'', '"', '`']).to_ascii_lowercase();
    if name == "utf8" {
        "utf8mb3".to_owned()
    } else if let Some(rest) = name.strip_prefix("utf8_") {
        format!("utf8mb3_{rest}")
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_character_sets_by_any_spelling() {
        assert_eq!(Charset::named("UTF8MB4").unwrap().max_bytes, 4);
        assert_eq!(Charset::named("'latin1'").unwrap().max_bytes, 1);
        assert_eq!(Charset::named("utf8").unwrap().name, "utf8mb3");
        assert!(Charset::named("gbk").is_none());
    }

    #[test]
    fn a_collation_names_its_character_set() {
        assert_eq!(
            Charset::of_collation("utf8mb4_unicode_ci").unwrap().name,
            "utf8mb4"
        );
        assert_eq!(
            Charset::of_collation("utf8_general_ci").unwrap().name,
            "utf8mb3"
        );
        assert_eq!(Charset::of_collation("binary").unwrap().name, "binary");
        assert_eq!(canonical("utf8_general_ci"), "utf8mb3_general_ci");
    }
}
