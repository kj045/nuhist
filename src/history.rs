//! Read-only access to nushell's history database.

use crate::Result;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Most recent command starting with a prefix, newest first.
const LATEST_PREFIX: &str = "
    SELECT command_line
    FROM history
    WHERE instr(command_line, ?1) = 1
    ORDER BY id DESC
    LIMIT 1
";

/// Same as [`LATEST_PREFIX`], but restricted to one working directory.
const LATEST_PREFIX_IN_CWD: &str = "
    SELECT command_line
    FROM history
    WHERE instr(command_line, ?1) = 1 AND cwd = ?2
    ORDER BY id DESC
    LIMIT 1
";

/// A read-only connection to nushell's SQLite history.
pub struct History {
    conn: Connection,
    path: PathBuf,
}

impl History {
    /// Open `path` read-only.
    ///
    /// If the read-only open fails because SQLite has to recover a hot WAL
    /// (which it cannot do read-only), fall back to a read-write connection
    /// with `PRAGMA query_only` set, so this process still never writes.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = match Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(conn) => conn,
            Err(read_only_err) => {
                let conn = Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .map_err(|_| read_only_err)?;
                conn.pragma_update(None, "query_only", true)?;
                conn
            }
        };

        conn.busy_timeout(Duration::from_millis(500))?;

        // Fail loudly when this is not a nushell SQLite history database.
        conn.query_row("SELECT command_line, cwd FROM history LIMIT 1", [], |_| {
            Ok(())
        })
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(()),
            other => Err(other),
        })?;

        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    /// The completion suffix for `line`, if any history entry starts with it.
    ///
    /// Matching mirrors nushell's built-in cwd-aware hinter: the most recent
    /// entry recorded in `cwd` wins; only when there is none does the search
    /// fall back to the most recent entry from any directory. Matching is
    /// case sensitive and literal (no `LIKE` wildcards).
    pub fn hint(&self, line: &str, cwd: Option<&str>) -> Result<Option<String>> {
        if line.is_empty() {
            return Ok(None);
        }

        if let Some(cwd) = cwd
            && let Some(command) = self.latest(line, Some(cwd))?
        {
            return Ok(Some(suffix(line, &command)));
        }

        Ok(self
            .latest(line, None)?
            .map(|command| suffix(line, &command)))
    }

    /// Total number of history rows.
    pub fn entries(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM history", [], |row| row.get(0))?)
    }

    /// Path of the underlying database.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn latest(&self, prefix: &str, cwd: Option<&str>) -> Result<Option<String>> {
        let mut statement = match cwd {
            Some(_) => self.conn.prepare_cached(LATEST_PREFIX_IN_CWD)?,
            None => self.conn.prepare_cached(LATEST_PREFIX)?,
        };

        let command = match cwd {
            Some(cwd) => statement
                .query_row(params![prefix, cwd], |row| row.get(0))
                .optional()?,
            None => statement
                .query_row(params![prefix], |row| row.get(0))
                .optional()?,
        };

        Ok(command)
    }
}

fn suffix(line: &str, command: &str) -> String {
    command.strip_prefix(line).unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(rows: &[(&str, Option<&str>)]) -> History {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                command_line TEXT NOT NULL,
                cwd TEXT
            ) STRICT;",
        )
        .unwrap();
        for (command, cwd) in rows {
            conn.execute(
                "INSERT INTO history (command_line, cwd) VALUES (?1, ?2)",
                params![command, cwd],
            )
            .unwrap();
        }
        History {
            conn,
            path: PathBuf::from(":memory:"),
        }
    }

    fn hint(history: &History, line: &str, cwd: Option<&str>) -> Option<String> {
        history.hint(line, cwd).unwrap()
    }

    #[test]
    fn returns_the_most_recent_global_match() {
        let db = history(&[("git status", None), ("git commit --amend", None)]);
        assert_eq!(hint(&db, "git ", None).as_deref(), Some("commit --amend"));
    }

    #[test]
    fn prefers_a_cwd_match_over_a_newer_global_match() {
        let db = history(&[
            ("cargo build", Some("/project")),
            ("cargo test", Some("/elsewhere")),
        ]);
        assert_eq!(
            hint(&db, "cargo ", Some("/project")).as_deref(),
            Some("build")
        );
        assert_eq!(hint(&db, "cargo ", None).as_deref(), Some("test"));
    }

    #[test]
    fn falls_back_to_global_history_without_a_cwd_match() {
        let db = history(&[("cargo test", Some("/elsewhere"))]);
        assert_eq!(
            hint(&db, "cargo ", Some("/project")).as_deref(),
            Some("test")
        );
    }

    #[test]
    fn rows_without_a_cwd_never_match_the_cwd_search() {
        let db = history(&[("cargo build", None)]);
        assert_eq!(
            hint(&db, "cargo ", Some("/project")).as_deref(),
            Some("build")
        );
    }

    #[test]
    fn exact_match_yields_an_empty_hint_instead_of_falling_through() {
        let db = history(&[
            ("git status", Some("/project")),
            ("git status --short", Some("/elsewhere")),
        ]);
        assert_eq!(
            hint(&db, "git status", Some("/project")).as_deref(),
            Some("")
        );
    }

    #[test]
    fn no_match_is_none() {
        let db = history(&[("git status", None)]);
        assert_eq!(hint(&db, "docker ", None), None);
    }

    #[test]
    fn empty_line_is_none() {
        let db = history(&[("git status", None)]);
        assert_eq!(hint(&db, "", None), None);
    }

    #[test]
    fn matching_is_case_sensitive_and_literal() {
        let db = history(&[("Git status", None)]);
        assert_eq!(hint(&db, "git", None), None);

        let db = history(&[("git status", None)]);
        assert_eq!(hint(&db, "git%", None), None);
    }

    #[test]
    fn handles_multibyte_prefixes() {
        let db = history(&[("café latte", None)]);
        assert_eq!(hint(&db, "café", None).as_deref(), Some(" latte"));
    }

    #[test]
    fn handles_multiline_commands() {
        let db = history(&[("echo one\necho two", None)]);
        assert_eq!(hint(&db, "echo one", None).as_deref(), Some("\necho two"));
    }
}
