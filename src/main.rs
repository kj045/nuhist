use nuhist::{
    Error, client, daemon, files, paths,
    protocol::{Request, Response},
};
use std::{
    env,
    io::Write,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

fn main() {
    if let Err(err) = run() {
        eprintln!("nuhist: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Error> {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        print_usage();
        return Ok(());
    };
    let rest = &args[1..];

    match command {
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        "daemon" => daemon_cmd(rest),
        "status" => status_cmd(),
        "stop" => stop_cmd(),
        "hint" => hint_cmd(rest),
        // `nuhist 'git '` is the original one-argument form, still handy for
        // keybindings and scripts.
        line if !line.starts_with('-') => hint(line, None),
        other => Err(Error::Usage(format!("unknown command: {other}"))),
    }
}

fn print_usage() {
    println!(
        "\
nuhist - shared nushell history hints

usage:
  nuhist hint [--line <LINE>] [--cwd <CWD>]   print the completion suffix for LINE
  nuhist <LINE>                               shorthand for `nuhist hint --line <LINE>`
  nuhist daemon [--history <PATH>] [--socket <PATH>] [--idle-timeout <SECS>] [--file-hints <MODE>]
  nuhist status                               show daemon status
  nuhist stop                                 stop the daemon

environment:
  NUHIST_HISTORY     path to nushell's history.sqlite3
  NUHIST_SOCKET      unix socket used by the daemon
  NUHIST_FILE_HINTS  off, path, args (default), or all"
    );
}

fn hint_cmd(args: &[String]) -> Result<(), Error> {
    let mut line: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--line" | "-l" => line = Some(next_value(args, &mut index, "--line")?),
            "--cwd" => cwd = Some(next_value(args, &mut index, "--cwd")?),
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(Error::Usage(format!("unknown option: {other}")));
            }
            other => {
                if line.replace(other.to_string()).is_some() {
                    return Err(Error::Usage("hint accepts only one line".into()));
                }
            }
        }
        index += 1;
    }

    match line {
        Some(line) => hint(&line, cwd.as_deref()),
        None => Err(Error::Usage("hint requires a line".into())),
    }
}

/// Print the hint suffix (no trailing newline). Failures are silent so that
/// hinting can never get in the way of the prompt.
fn hint(line: &str, cwd: Option<&str>) -> Result<(), Error> {
    if line.is_empty() {
        return Ok(());
    }

    let (Ok(socket), Ok(history)) = (paths::socket_path(), paths::history_path()) else {
        return Ok(());
    };

    let cwd = cwd.map(str::to_string).or_else(|| {
        env::current_dir()
            .ok()
            .map(|dir| dir.to_string_lossy().into_owned())
    });
    let request = Request::Hint {
        line: line.to_string(),
        cwd,
    };

    if let Ok(Response::Hint { hint }) = client::request_or_spawn(&socket, &history, &request) {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(hint.as_bytes())?;
        stdout.flush()?;
    }

    Ok(())
}

fn daemon_cmd(args: &[String]) -> Result<(), Error> {
    let mut history: Option<PathBuf> = None;
    let mut socket: Option<PathBuf> = None;
    let mut idle_timeout: Option<Duration> = None;
    let mut file_hints: Option<files::Mode> = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--history" => {
                history = Some(PathBuf::from(next_value(args, &mut index, "--history")?))
            }
            "--socket" => socket = Some(PathBuf::from(next_value(args, &mut index, "--socket")?)),
            "--idle-timeout" => {
                let secs: u64 = next_value(args, &mut index, "--idle-timeout")?
                    .parse()
                    .map_err(|_| {
                        Error::Usage("--idle-timeout expects a number of seconds".into())
                    })?;
                idle_timeout = (secs > 0).then(|| Duration::from_secs(secs));
            }
            "--file-hints" => {
                let value = next_value(args, &mut index, "--file-hints")?;
                file_hints = Some(value.parse().map_err(|_| {
                    Error::Usage("--file-hints expects one of: off, path, args, all".into())
                })?);
            }
            other => return Err(Error::Usage(format!("unknown daemon option: {other}"))),
        }
        index += 1;
    }

    let config = daemon::Config {
        history: history.map_or_else(paths::history_path, Ok)?,
        socket: socket.map_or_else(paths::socket_path, Ok)?,
        idle_timeout,
        file_hints: file_hints.unwrap_or_else(files::Mode::from_env),
    };

    daemon::run(config, Arc::new(AtomicBool::new(false)))
}

fn status_cmd() -> Result<(), Error> {
    let socket = paths::socket_path()?;

    match client::request(&socket, &Request::Status) {
        Ok(Response::Status {
            pid,
            history_path,
            socket_path,
            entries,
            file_hints,
            uptime_secs,
        }) => {
            println!("daemon: running (pid {pid})");
            println!("socket: {socket_path}");
            println!("history: {history_path} ({entries} entries)");
            println!("files: {file_hints}");
            println!("uptime: {uptime_secs}s");
        }
        Ok(Response::Error { message }) => println!("daemon: error: {message}"),
        Ok(_) => println!("daemon: unexpected response"),
        Err(err) => {
            println!("daemon: not running ({err})");
            match paths::history_path() {
                Ok(path) => {
                    println!("history: {}", path.display());
                    if !path.exists() {
                        println!(
                            "  history file is missing; nushell must use `history.file_format = \"sqlite\"`"
                        );
                    }
                }
                Err(err) => println!("history: {err}"),
            }
            if let Some(log) = paths::log_path() {
                println!("log: {}", log.display());
            }
        }
    }

    Ok(())
}

fn stop_cmd() -> Result<(), Error> {
    let socket = paths::socket_path()?;

    match client::request(&socket, &Request::Shutdown) {
        Ok(_) => {
            // Wait until the socket is gone, so scripts can rely on the daemon
            // being stopped once this returns.
            for _ in 0..100 {
                if std::os::unix::net::UnixStream::connect(&socket).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            println!("daemon: stopped");
        }
        Err(_) => println!("daemon: not running"),
    }

    Ok(())
}

fn next_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, Error> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| Error::Usage(format!("{flag} requires a value")))
}
