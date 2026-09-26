//! End-to-end tests for the hint daemon.

use nuhist::{
    client, daemon, files,
    protocol::{Request, Response},
};
use rusqlite::{Connection, params};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// A unique temporary directory that cleans itself up.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nuhist-test-{name}-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn make_history(path: &Path, rows: &[(&str, &str)]) {
    let conn = Connection::open(path).unwrap();
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
}

fn start_daemon(socket: &Path, history: &Path, idle_timeout: Option<Duration>) -> JoinHandle<()> {
    let config = daemon::Config {
        history: history.to_path_buf(),
        socket: socket.to_path_buf(),
        idle_timeout,
        file_hints: files::Mode::Args,
    };
    let shutdown = Arc::new(AtomicBool::new(false));

    thread::spawn(move || daemon::run(config, shutdown).unwrap())
}

fn wait_for_daemon(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if client::request(socket, &Request::Status).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("daemon did not start");
}

fn ask(socket: &Path, line: &str, cwd: &str) -> String {
    let request = Request::Hint {
        line: line.to_string(),
        cwd: Some(cwd.to_string()),
    };
    match client::request(socket, &request).unwrap() {
        Response::Hint { hint } => hint,
        other => panic!("unexpected response: {other:?}"),
    }
}

fn shutdown(socket: &Path) {
    match client::request(socket, &Request::Shutdown).unwrap() {
        Response::Done { .. } => {}
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn serves_cwd_scoped_hints_over_the_socket() {
    let dir = TempDir::new("cwd");
    let history = dir.path().join("history.sqlite3");
    make_history(
        &history,
        &[
            ("cargo build --release", "/project"),
            ("cargo test", "/elsewhere"),
            ("git status", "/project"),
            ("git status --short", "/elsewhere"),
        ],
    );

    let socket = dir.path().join("nuhist.sock");
    let handle = start_daemon(&socket, &history, None);
    wait_for_daemon(&socket);

    // A same-cwd match wins, even when it is older than a global match.
    assert_eq!(ask(&socket, "cargo ", "/project"), "build --release");
    // Without a same-cwd match, fall back to the most recent global match.
    assert_eq!(ask(&socket, "cargo ", "/other"), "test");
    // An exact same-cwd match stops the search instead of falling through.
    assert_eq!(ask(&socket, "git status", "/project"), "");
    // No match at all.
    assert_eq!(ask(&socket, "docker ", "/project"), "");

    match client::request(&socket, &Request::Status).unwrap() {
        Response::Status { entries, .. } => assert_eq!(entries, 4),
        other => panic!("unexpected response: {other:?}"),
    }

    shutdown(&socket);
    handle.join().unwrap();
    assert!(!socket.exists());
}

#[test]
fn falls_back_to_file_suggestions_when_history_has_no_match() {
    let dir = TempDir::new("files");
    let history = dir.path().join("history.sqlite3");
    make_history(
        &history,
        &[("git status", "/project"), ("cd assets ", "/project")],
    );

    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::create_dir_all(dir.path().join("assets")).unwrap();
    fs::write(dir.path().join("README.md"), "").unwrap();

    let socket = dir.path().join("nuhist.sock");
    let handle = start_daemon(&socket, &history, None);
    wait_for_daemon(&socket);

    let cwd = dir.path().to_string_lossy().into_owned();

    // No history entry starts with the line, so files are suggested.
    assert_eq!(ask(&socket, "cat REA", &cwd), "DME.md");
    // Directories get a trailing slash.
    assert_eq!(ask(&socket, "cd sr", &cwd), "c/");
    // History still wins over files when it matches.
    assert_eq!(ask(&socket, "git ", &cwd), "status");
    // Exact file names are already complete.
    assert_eq!(ask(&socket, "cat README.md", &cwd), "");
    // A history match with only a whitespace suffix is invisible, so the file
    // suggestion (`assets/`) wins.
    assert_eq!(ask(&socket, "cd assets", &cwd), "/");

    shutdown(&socket);
    handle.join().unwrap();
}

#[test]
fn a_second_daemon_leaves_the_first_one_alone() {
    let dir = TempDir::new("lock");
    let history = dir.path().join("history.sqlite3");
    make_history(&history, &[("git push origin main", "/project")]);

    let socket = dir.path().join("nuhist.sock");
    let handle = start_daemon(&socket, &history, None);
    wait_for_daemon(&socket);

    // The lock must make this return immediately instead of taking over.
    let config = daemon::Config {
        history: history.clone(),
        socket: socket.clone(),
        idle_timeout: None,
        file_hints: files::Mode::Args,
    };
    daemon::run(config, Arc::new(AtomicBool::new(false))).unwrap();

    assert_eq!(ask(&socket, "git ", "/project"), "push origin main");

    shutdown(&socket);
    handle.join().unwrap();
}

#[test]
fn replaces_a_stale_socket_and_restarts_after_stop() {
    let dir = TempDir::new("stale");
    let history = dir.path().join("history.sqlite3");
    make_history(&history, &[("git status", "/project")]);
    let socket = dir.path().join("nuhist.sock");

    // Simulate a crashed daemon: a socket file nobody listens on.
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());

    let handle = start_daemon(&socket, &history, None);
    wait_for_daemon(&socket);
    assert_eq!(ask(&socket, "git ", "/project"), "status");
    shutdown(&socket);
    handle.join().unwrap();

    // The lock must be released, so a fresh daemon can take over.
    let handle = start_daemon(&socket, &history, None);
    wait_for_daemon(&socket);
    assert_eq!(ask(&socket, "git ", "/project"), "status");
    shutdown(&socket);
    handle.join().unwrap();
}

#[test]
fn idle_timeout_stops_the_daemon() {
    let dir = TempDir::new("idle");
    let history = dir.path().join("history.sqlite3");
    make_history(&history, &[("git status", "/project")]);

    let socket = dir.path().join("nuhist.sock");
    let handle = start_daemon(&socket, &history, Some(Duration::from_millis(150)));
    wait_for_daemon(&socket);

    handle.join().unwrap();
    assert!(!socket.exists());
}

#[test]
fn cli_spawns_one_daemon_and_reuses_it() {
    let dir = TempDir::new("cli");
    let history = dir.path().join("history.sqlite3");
    make_history(
        &history,
        &[
            ("git push origin main", "/project"),
            ("cargo build", "/project"),
        ],
    );

    let socket = dir.path().join("nuhist.sock");
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();

    let nuhist = env!("CARGO_BIN_EXE_nuhist");
    let run = |args: &[&str]| {
        std::process::Command::new(nuhist)
            .args(args)
            .env("NUHIST_HISTORY", &history)
            .env("NUHIST_SOCKET", &socket)
            .env("NUHIST_FILE_HINTS", "off")
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join(".state"))
            .output()
            .unwrap()
    };

    let first = run(&["hint", "--line", "git ", "--cwd", "/project"]);
    assert!(first.status.success());
    assert_eq!(String::from_utf8(first.stdout).unwrap(), "push origin main");

    // The second call must reuse the daemon rather than racing to start one.
    let second = run(&["hint", "cargo", "--cwd", "/project"]);
    assert!(second.status.success());
    assert_eq!(String::from_utf8(second.stdout).unwrap(), " build");

    // `NUHIST_FILE_HINTS=off` reaches the spawned daemon: a line with no
    // history match must not fall back to files.
    fs::write(dir.path().join("README.md"), "").unwrap();
    let files_off = run(&[
        "hint",
        "--line",
        "cat REA",
        "--cwd",
        &dir.path().to_string_lossy(),
    ]);
    assert!(files_off.status.success());
    assert_eq!(String::from_utf8(files_off.stdout).unwrap(), "");

    let status = run(&["status"]);
    let status = String::from_utf8(status.stdout).unwrap();
    assert!(status.contains("running"), "unexpected status: {status}");
    assert!(status.contains("2 entries"), "unexpected status: {status}");
    assert!(status.contains("files: off"), "unexpected status: {status}");

    let stop = run(&["stop"]);
    assert!(String::from_utf8(stop.stdout).unwrap().contains("stopped"));
    assert!(!socket.exists());
}
