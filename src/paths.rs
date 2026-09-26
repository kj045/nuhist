//! Default locations for the history database, the daemon socket and the log.

use crate::{Error, Result};
use std::{env, path::PathBuf};

/// Path to nushell's SQLite history database.
pub fn history_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("NUHIST_HISTORY").or_else(|| env::var_os("NU_HISTORY_PATH")) {
        return Ok(PathBuf::from(path));
    }

    let config_dir = match env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            let home = env::var_os("HOME")
                .ok_or_else(|| Error::Usage("HOME is not set; set NUHIST_HISTORY".into()))?;
            PathBuf::from(home).join(".config")
        }
    };

    Ok(config_dir.join("nushell").join("history.sqlite3"))
}

/// Path to the daemon's unix socket.
pub fn socket_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("NUHIST_SOCKET") {
        return Ok(PathBuf::from(path));
    }

    let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| Error::Usage("XDG_RUNTIME_DIR is not set; set NUHIST_SOCKET".into()))?;
    Ok(PathBuf::from(runtime_dir).join("nuhist.sock"))
}

/// Path to the daemon log file, if a state directory can be determined.
pub fn log_path() -> Option<PathBuf> {
    let state_dir = match env::var_os("XDG_STATE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(env::var_os("HOME")?)
            .join(".local")
            .join("state"),
    };
    Some(state_dir.join("nuhist").join("nuhist.log"))
}
