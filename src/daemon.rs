//! The hint daemon: one per login session, shared by every nushell session.

use crate::{
    Error, Result, files,
    history::History,
    protocol::{self, Request, Response},
};
use std::{
    fs::{self, File, TryLockError},
    io::{self, BufReader, ErrorKind},
    os::{
        fd::AsRawFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// How long the accept loop sleeps between non-blocking polls.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// Read/write timeout for a single client.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct Config {
    pub history: PathBuf,
    pub socket: PathBuf,
    /// Shut down after this long without requests. `None` keeps the daemon
    /// alive until logout or `nuhist stop`.
    pub idle_timeout: Option<Duration>,
    /// How eagerly to suggest filesystem entries when history has no match.
    pub file_hints: files::Mode,
}

/// Serve hint requests until `shutdown` is set or the idle timeout fires.
///
/// Returns `Ok(())` immediately when another daemon already owns the socket.
pub fn run(config: Config, shutdown: Arc<AtomicBool>) -> Result<()> {
    let _lock = match try_lock(&config.socket) {
        Ok(lock) => lock,
        Err(LockError::Held) => return Ok(()),
        Err(LockError::Io(err)) => return Err(err.into()),
    };

    let history = History::open(&config.history)
        .map_err(|err| Error::History(format!("{}: {err}", config.history.display())))?;

    if let Some(parent) = config.socket.parent() {
        fs::create_dir_all(parent)?;
    }
    // We hold the lock, so an existing socket file must be stale.
    if config.socket.exists() {
        fs::remove_file(&config.socket)?;
    }
    let listener = UnixListener::bind(&config.socket)?;
    listener.set_nonblocking(true)?;

    let started = Instant::now();
    let mut last_activity = Instant::now();
    let mut file_cache = files::DirCache::new();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Wake up as soon as a client connects, but never sleep longer than
        // POLL_INTERVAL so shutdown and idle checks stay responsive.
        match wait_for_connection(&listener, POLL_INTERVAL) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => {
                eprintln!("nuhist: poll error: {err}");
                continue;
            }
        }

        match listener.accept() {
            Ok((stream, _)) => {
                last_activity = Instant::now();
                if let Err(err) = serve(
                    &stream,
                    &history,
                    &mut file_cache,
                    &config,
                    started,
                    &shutdown,
                ) {
                    eprintln!("nuhist: client error: {err}");
                }
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {}
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => eprintln!("nuhist: accept error: {err}"),
        }

        if let Some(idle) = config.idle_timeout
            && last_activity.elapsed() >= idle
        {
            break;
        }
    }

    drop(listener);
    let _ = fs::remove_file(&config.socket);
    Ok(())
}

fn wait_for_connection(listener: &UnixListener, timeout: Duration) -> io::Result<()> {
    let mut pollfd = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;

    // SAFETY: `pollfd` is a single initialized element and `poll` does not
    // retain the pointer.
    let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if ready < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn serve(
    stream: &UnixStream,
    history: &History,
    file_cache: &mut files::DirCache,
    config: &Config,
    started: Instant,
    shutdown: &AtomicBool,
) -> Result<()> {
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let request: Request = protocol::read_message(&mut reader)?;

    let response = match request {
        Request::Hint { line, cwd } => {
            // History first, exactly like nushell's built-in hinter. Files are
            // the fallback, so an exact history match still ends the search.
            let hint = match history.hint(&line, cwd.as_deref()) {
                Ok(Some(hint)) if hint.is_empty() || !hint.trim().is_empty() => Some(hint),
                // A suffix of only whitespace is invisible; let files try.
                Ok(Some(_)) => None,
                Ok(None) => None,
                Err(err) => {
                    eprintln!("nuhist: history error: {err}");
                    None
                }
            };

            let hint = hint.or_else(|| {
                cwd.as_deref().and_then(|cwd| {
                    files::hint(&line, Path::new(cwd), config.file_hints, file_cache)
                })
            });

            Response::Hint {
                hint: hint.unwrap_or_default(),
            }
        }
        Request::Status => Response::Status {
            pid: std::process::id(),
            history_path: config.history.display().to_string(),
            socket_path: config.socket.display().to_string(),
            entries: history.entries().unwrap_or(-1),
            file_hints: config.file_hints.as_str().to_string(),
            uptime_secs: started.elapsed().as_secs(),
        },
        Request::Shutdown => {
            shutdown.store(true, Ordering::Relaxed);
            Response::Done {
                message: "shutting down".to_string(),
            }
        }
    };

    protocol::write_message(&mut &*stream, &response)?;
    Ok(())
}

enum LockError {
    Held,
    Io(std::io::Error),
}

/// Take an exclusive lock next to the socket, so only one daemon can run.
///
/// The lock file is intentionally never deleted: unlinking a locked file would
/// let a second daemon lock a fresh inode with the same name.
fn try_lock(socket: &Path) -> std::result::Result<File, LockError> {
    let lock_path = socket.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(LockError::Io)?;
    }
    let file = File::create(&lock_path).map_err(LockError::Io)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(LockError::Held),
        Err(TryLockError::Error(err)) => Err(LockError::Io(err)),
    }
}
