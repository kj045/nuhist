//! The line-oriented JSON protocol spoken between the hint client and the
//! daemon.
//!
//! Both sides talk over a unix socket using one request and one response per
//! connection. JSON keeps arbitrary command lines (including multi-line
//! commands) safe to send.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, BufRead, Write};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Ask for the text that completes `line`, preferring commands previously
    /// run in `cwd`.
    Hint {
        line: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// Ask for daemon diagnostics.
    Status,
    /// Ask the daemon to shut down cleanly.
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// The completion suffix for the requested line. Empty when nothing matched.
    Hint {
        hint: String,
    },
    Status {
        pid: u32,
        history_path: String,
        socket_path: String,
        entries: i64,
        file_hints: String,
        uptime_secs: u64,
    },
    Done {
        message: String,
    },
    Error {
        message: String,
    },
}

/// Write one newline-terminated JSON message.
pub fn write_message<W, T>(writer: &mut W, value: &T) -> io::Result<()>
where
    W: Write,
    T: Serialize,
{
    serde_json::to_writer(&mut *writer, value).map_err(io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Read one newline-terminated JSON message.
pub fn read_message<R, T>(reader: &mut R) -> io::Result<T>
where
    R: BufRead,
    T: DeserializeOwned,
{
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty message",
        ));
    }
    serde_json::from_str(&line).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn requests_round_trip_with_multiline_commands() {
        let request = Request::Hint {
            line: "echo one\necho two".to_string(),
            cwd: Some("/home/me".to_string()),
        };

        let mut buffer = Vec::new();
        write_message(&mut buffer, &request).unwrap();

        let decoded: Request = read_message(&mut BufReader::new(&buffer[..])).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn responses_round_trip_with_unicode() {
        let response = Response::Hint {
            hint: " café".to_string(),
        };

        let mut buffer = Vec::new();
        write_message(&mut buffer, &response).unwrap();

        let decoded: Response = read_message(&mut BufReader::new(&buffer[..])).unwrap();
        assert_eq!(decoded, response);
    }
}
