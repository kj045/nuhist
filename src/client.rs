//! Talking to the hint daemon, including lazy startup.

use crate::{
    paths,
    protocol::{self, Request, Response},
};
use std::{
    fs::{self, OpenOptions},
    io::{self, BufReader, ErrorKind},
    os::unix::net::UnixStream,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Read/write timeout for a single request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// How long to wait for a freshly spawned daemon to accept requests.
const SPAWN_RETRY: Duration = Duration::from_millis(300);
const RETRY_INTERVAL: Duration = Duration::from_millis(5);

/// Send one request to the daemon without starting it.
pub fn request(socket: &Path, request: &Request) -> io::Result<Response> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;

    protocol::write_message(&mut stream, request)?;
    protocol::read_message(&mut BufReader::new(stream))
}

/// Send a request, starting the daemon first if it is not running.
///
/// Every session does this, but the daemon's lock file guarantees that a
/// single daemon wins even when several sessions race to start one.
pub fn request_or_spawn(socket: &Path, history: &Path, message: &Request) -> io::Result<Response> {
    match request(socket, message) {
        Ok(response) => Ok(response),
        Err(connect_err) => {
            spawn(socket, history)?;

            let deadline = Instant::now() + SPAWN_RETRY;
            let mut last_err = connect_err;
            while Instant::now() < deadline {
                match request(socket, message) {
                    Ok(response) => return Ok(response),
                    Err(err) => last_err = err,
                }
                thread::sleep(RETRY_INTERVAL);
            }
            Err(last_err)
        }
    }
}

/// Spawn a detached daemon for this binary, logging to the state directory.
fn spawn(socket: &Path, history: &Path) -> io::Result<()> {
    if !history.exists() {
        return Err(io::Error::new(
            ErrorKind::NotFound,
            format!("history database not found: {}", history.display()),
        ));
    }

    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .arg("daemon")
        .arg("--history")
        .arg(history)
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log_stdio());
    command.spawn()?;
    Ok(())
}

fn log_stdio() -> Stdio {
    let Some(path) = paths::log_path() else {
        return Stdio::null();
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => Stdio::from(file),
        Err(_) => Stdio::null(),
    }
}
