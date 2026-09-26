//! A small nushell history + hints utility.
//!
//! A single daemon per login session answers hint requests from every nushell
//! session. Hints are read straight from nushell's SQLite history database, so
//! all sessions share the same, synchronized history, and a hint can prefer
//! commands that were previously run in the current working directory.

pub mod client;
pub mod daemon;
pub mod files;
pub mod history;
pub mod paths;
pub mod protocol;

use std::fmt;

/// Crate-wide error type.
///
/// The hint path never surfaces these to the shell: if the daemon cannot be
/// reached, the client simply prints nothing.
#[derive(Debug)]
pub enum Error {
    /// Filesystem or socket errors.
    Io(std::io::Error),
    /// Problems reading nushell's history database.
    History(String),
    /// Malformed daemon protocol traffic.
    Protocol(String),
    /// Bad command line usage.
    Usage(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(err) => write!(f, "{err}"),
            Error::History(message) => write!(f, "{message}"),
            Error::Protocol(message) => write!(f, "{message}"),
            Error::Usage(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        Error::History(err.to_string())
    }
}

/// Shorthand for crate results.
pub type Result<T> = std::result::Result<T, Error>;
