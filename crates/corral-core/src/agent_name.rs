//! An agent's name (M4 item 6; TP-ANM-01). The name is also its folder, `~/Corral/<name>`, the
//! sandbox its file tools work in, so a name is one plain path component that can't be mistaken
//! for another agent's:
//!
//! - ASCII letters and digits, spaces, `-` and `_`: no separators, dots (so never `..` or a
//!   hidden folder), control or direction characters, and no Unicode, whose composed and
//!   decomposed forms the Mac's file system treats as one name;
//! - starting and ending with a letter or digit, trimmed, at most [`MAX_NAME_CHARS`];
//! - names differing only in case are the same name, as they are on the Mac's file system.

/// The longest name.
pub const MAX_NAME_CHARS: usize = 32;

/// Why a name was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    /// Nothing but spaces.
    #[error("A name can't be empty.")]
    Empty,
    /// Over [`MAX_NAME_CHARS`].
    #[error("A name can be at most 32 characters.")]
    TooLong,
    /// A character a name can't have.
    #[error("A name can't have {0:?}: it can use letters A to Z, digits, spaces, - and _.")]
    Character(char),
    /// A space, `-` or `_` at either end.
    #[error("A name has to start and end with a letter or digit.")]
    Edge,
}

/// A name an agent can have.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentName(String);

impl AgentName {
    /// `name`, trimmed, if it's one an agent can have.
    pub fn new(name: &str) -> Result<Self, NameError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(NameError::Empty);
        }
        if let Some(bad) = name
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_')))
        {
            return Err(NameError::Character(bad));
        }
        if name.len() > MAX_NAME_CHARS {
            return Err(NameError::TooLong);
        }
        let edge_ok = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
        if !edge_ok(name.chars().next()) || !edge_ok(name.chars().last()) {
            return Err(NameError::Edge);
        }
        Ok(Self(name.to_owned()))
    }

    /// The name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `other`, trimmed, is the same name, case aside.
    pub fn same_as(&self, other: &str) -> bool {
        self.0.eq_ignore_ascii_case(other.trim())
    }

    /// The first letter of its first two words, upper case: "Muse Spark" is "MS".
    pub fn monogram(&self) -> String {
        self.0
            .split(' ')
            .filter_map(|word| word.chars().next())
            .take(2)
            .map(|c| c.to_ascii_uppercase())
            .collect()
    }
}

impl std::fmt::Display for AgentName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
