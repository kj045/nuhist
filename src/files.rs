//! fish-style filesystem completions for the last token on the line.
//!
//! The daemon only ever returns the *suffix* to append to the current command
//! line, so completion is limited to names that can be appended safely:
//! bare names must contain only characters that need no quoting, and quoted
//! tokens get their closing quote back.

use std::{
    env,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant},
};

/// How eagerly the daemon suggests files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Never suggest files.
    Off,
    /// Only for path-like tokens (`./file`, `src/file`, `~/file`, `.hidden`).
    Path,
    /// Path-like tokens anywhere, plus bare names in argument position.
    #[default]
    Args,
    /// Bare names everywhere, including command position.
    All,
}

impl FromStr for Mode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "off" | "none" | "false" | "no" => Ok(Mode::Off),
            "path" | "paths" => Ok(Mode::Path),
            "args" | "arguments" | "on" | "true" | "yes" => Ok(Mode::Args),
            "all" | "always" => Ok(Mode::All),
            _ => Err(()),
        }
    }
}

impl Mode {
    /// Read `NUHIST_FILE_HINTS`, defaulting to [`Mode::Args`].
    pub fn from_env() -> Self {
        env::var("NUHIST_FILE_HINTS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or_default()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Path => "path",
            Mode::Args => "args",
            Mode::All => "all",
        }
    }
}

/// The suffix that completes the last token of `line` with a filesystem entry.
pub fn hint(line: &str, cwd: &Path, mode: Mode, cache: &mut DirCache) -> Option<String> {
    let home = env::var_os("HOME").map(PathBuf::from);
    hint_with_home(line, cwd, mode, home.as_deref(), cache)
}

fn hint_with_home(
    line: &str,
    cwd: &Path,
    mode: Mode,
    home: Option<&Path>,
    cache: &mut DirCache,
) -> Option<String> {
    if mode == Mode::Off {
        return None;
    }

    let (token, before) = last_token(line)?;
    let path_like = is_path_like(token);
    let allowed = match mode {
        Mode::Off => false,
        Mode::Path => path_like,
        Mode::Args => path_like || !before.trim().is_empty(),
        Mode::All => true,
    };
    if !allowed {
        return None;
    }

    complete(token, cwd, home, cache)
}

/// Split off the token at the end of the line, plus everything before it.
///
/// A trailing whitespace means the user just started a new token, and an
/// unterminated quote keeps the whole quoted argument together.
fn last_token(line: &str) -> Option<(&str, &str)> {
    if line.is_empty() || line.chars().next_back().is_some_and(char::is_whitespace) {
        return None;
    }

    let mut start = 0;
    let mut quote: Option<char> = None;
    for (index, character) in line.char_indices() {
        match quote {
            Some(open) => {
                if character == open {
                    quote = None;
                }
            }
            None => {
                if (character == '"' || character == '\'') && index == start {
                    quote = Some(character);
                } else if character.is_whitespace() {
                    start = index + character.len_utf8();
                }
            }
        }
    }

    Some((&line[start..], &line[..start]))
}

fn is_path_like(token: &str) -> bool {
    let token = token.strip_prefix(['"', '\'']).unwrap_or(token);
    token.starts_with('.') || token.starts_with('~') || token.contains('/')
}

fn complete(token: &str, cwd: &Path, home: Option<&Path>, cache: &mut DirCache) -> Option<String> {
    let (quote, rest) = match token.chars().next() {
        Some(open @ ('"' | '\'')) => (Some(open), &token[1..]),
        _ => (None, token),
    };
    if rest.is_empty() {
        return None;
    }

    // Bare `.`, `..` and `~` are directories that complete to `.../`.
    if !rest.ends_with('/') && (rest == "~" || rest.chars().all(|c| c == '.')) {
        return resolve(rest, cwd, home)
            .filter(|dir| dir.is_dir())
            .map(|_| format!("/{}", closing_quote(quote)));
    }

    // Split the token into the literal directory part and the name prefix.
    let (base, name) = match rest.rfind('/') {
        Some(slash) => (&rest[..=slash], &rest[slash + 1..]),
        None => ("", rest),
    };

    let directory = resolve(base, cwd, home)?;
    let entries = cache.entries(&directory)?;

    let include_hidden = name.starts_with('.');
    let mut candidates: Vec<&Entry> = entries
        .iter()
        .filter(|entry| {
            (include_hidden || !entry.name.starts_with('.')) && entry.name.starts_with(name)
        })
        .collect();
    candidates.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });

    // An exact match ends the token; directories still get a trailing slash.
    if let Some(entry) = candidates.iter().find(|entry| entry.name == name) {
        return entry.is_dir.then(|| format!("/{}", closing_quote(quote)));
    }

    let entry = candidates
        .iter()
        .find(|entry| is_appendable(&entry.name, quote))?;

    let mut suffix = entry.name[name.len()..].to_string();
    if entry.is_dir {
        suffix.push('/');
    }
    if let Some(open) = quote {
        suffix.push(open);
    }
    Some(suffix)
}

/// Resolve an explicitly typed path fragment (not the name being completed).
fn resolve(base: &str, cwd: &Path, home: Option<&Path>) -> Option<PathBuf> {
    if base.is_empty() {
        return Some(cwd.to_path_buf());
    }
    if base == "~" {
        return Some(home?.to_path_buf());
    }
    if let Some(rest) = base.strip_prefix("~/") {
        return Some(home?.join(rest));
    }

    let path = Path::new(base);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    })
}

/// Nothing has to be escaped when appending `name` to the line.
fn is_appendable(name: &str, quote: Option<char>) -> bool {
    match quote {
        Some(open) => name
            .chars()
            .all(|c| !c.is_control() && c != open && c != '\\' && (open == '\'' || c != '$')),
        None => name
            .chars()
            .all(|c| c.is_alphanumeric() || "._-+@%:,=~".contains(c)),
    }
}

fn closing_quote(quote: Option<char>) -> &'static str {
    match quote {
        Some('"') => "\"",
        Some(_) => "'",
        None => "",
    }
}

#[derive(Debug)]
struct Entry {
    name: String,
    is_dir: bool,
}

/// A one-entry cache of directory listings, so typing does not hit the
/// filesystem once per keystroke.
#[derive(Debug, Default)]
pub struct DirCache {
    last: Option<(PathBuf, Instant, Vec<Entry>)>,
}

/// How long a directory listing stays valid.
const CACHE_TTL: Duration = Duration::from_millis(500);

impl DirCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&mut self, directory: &Path) -> Option<&[Entry]> {
        let stale = match &self.last {
            Some((path, read_at, _)) => path != directory || read_at.elapsed() >= CACHE_TTL,
            None => true,
        };
        if stale {
            self.last = Some((
                directory.to_path_buf(),
                Instant::now(),
                read_entries(directory)?,
            ));
        }
        self.last.as_ref().map(|(_, _, entries)| entries.as_slice())
    }
}

fn read_entries(directory: &Path) -> Option<Vec<Entry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(directory).ok()? {
        let Ok(entry) = entry else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let is_dir = entry.path().is_dir();
        entries.push(Entry { name, is_dir });
    }
    Some(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let path = env::temp_dir().join(format!(
                "nuhist-files-{name}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn file(&self, name: &str) {
            fs::write(self.0.join(name), "").unwrap();
        }

        fn dir(&self, name: &str) {
            fs::create_dir_all(self.0.join(name)).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(name: &str) -> Fixture {
        let fixture = Fixture::new(name);
        fixture.file("README.md");
        fixture.file("readme");
        fixture.file("notes.txt");
        fixture.file("notes2.txt");
        fixture.file("bad name.txt");
        fixture.file(".hidden");
        fixture.dir("src");
        fixture.dir("My Documents");
        fixture
    }

    fn hint(line: &str, cwd: &Path, mode: Mode) -> Option<String> {
        let mut cache = DirCache::new();
        let home = cwd.parent();
        hint_with_home(line, cwd, mode, home, &mut cache)
    }

    fn text(line: &str, cwd: &Path, mode: Mode) -> Option<String> {
        hint(line, cwd, mode)
    }

    #[test]
    fn completes_bare_names_in_argument_position() {
        let fixture = fixture("bare");
        assert_eq!(
            text("cat REA", fixture.path(), Mode::Args).as_deref(),
            Some("DME.md")
        );
        // Matching is case sensitive (a completion suffix cannot change case).
        assert_eq!(
            text("cat rea", fixture.path(), Mode::Args).as_deref(),
            Some("dme")
        );
    }

    #[test]
    fn path_mode_only_completes_path_like_tokens() {
        let fixture = fixture("path");
        fixture.file("src/notes.txt");
        assert_eq!(text("cat REA", fixture.path(), Mode::Path), None);
        assert_eq!(
            text("cat ./REA", fixture.path(), Mode::Path).as_deref(),
            Some("DME.md")
        );
        assert_eq!(
            text("cat src/no", fixture.path(), Mode::Path).as_deref(),
            Some("tes.txt")
        );
    }

    #[test]
    fn args_mode_skips_command_position_but_all_does_not() {
        let fixture = fixture("position");
        assert_eq!(text("REA", fixture.path(), Mode::Args), None);
        assert_eq!(
            text("REA", fixture.path(), Mode::All).as_deref(),
            Some("DME.md")
        );
    }

    #[test]
    fn directories_get_a_trailing_slash() {
        let fixture = fixture("dirs");
        assert_eq!(
            text("cd sr", fixture.path(), Mode::Args).as_deref(),
            Some("c/")
        );
        // An exact directory match still gets the slash.
        assert_eq!(
            text("cd src", fixture.path(), Mode::Args).as_deref(),
            Some("/")
        );
        // Typing the slash lists the directory's contents.
        fixture.file("src/lib.rs");
        assert_eq!(
            text("cat src/", fixture.path(), Mode::Args).as_deref(),
            Some("lib.rs")
        );
    }

    #[test]
    fn exact_files_are_already_complete() {
        let fixture = fixture("exact");
        assert_eq!(text("cat README.md", fixture.path(), Mode::Args), None);
    }

    #[test]
    fn hidden_entries_need_a_dot_prefix() {
        let fixture = fixture("hidden");
        assert_eq!(
            text("cat .hid", fixture.path(), Mode::Args).as_deref(),
            Some("den")
        );
        assert_eq!(text("cat hid", fixture.path(), Mode::Args), None);
    }

    #[test]
    fn dots_and_tilde_complete_to_directories() {
        let fixture = fixture("dots");
        assert_eq!(
            text("cd .", fixture.path(), Mode::Args).as_deref(),
            Some("/")
        );
        assert_eq!(
            text("cd ..", fixture.path(), Mode::Args).as_deref(),
            Some("/")
        );

        let home = Fixture::new("home");
        home.file("notes.txt");
        let mut cache = DirCache::new();
        assert_eq!(
            hint_with_home(
                "cat ~/no",
                fixture.path(),
                Mode::Args,
                Some(home.path()),
                &mut cache
            )
            .as_deref(),
            Some("tes.txt")
        );
        assert_eq!(
            hint_with_home(
                "cd ~",
                fixture.path(),
                Mode::Args,
                Some(home.path()),
                &mut cache
            )
            .as_deref(),
            Some("/")
        );
    }

    #[test]
    fn names_that_need_quoting_are_skipped_or_quoted() {
        let fixture = fixture("quoting");
        fixture.file("bat.txt");
        // `bad name.txt` cannot be appended to a bare token, so the next safe
        // candidate wins.
        assert_eq!(
            text("cat ba", fixture.path(), Mode::Args).as_deref(),
            Some("t.txt")
        );
        // Inside a quote there is nothing to escape, and we close it.
        assert_eq!(
            text("cat \"My Doc", fixture.path(), Mode::Args).as_deref(),
            Some("uments/\"")
        );
        assert_eq!(
            text("cat \"bad", fixture.path(), Mode::Args).as_deref(),
            Some(" name.txt\"")
        );
    }

    #[test]
    fn a_trailing_space_starts_a_new_token() {
        let fixture = fixture("space");
        assert_eq!(text("cat ", fixture.path(), Mode::Args), None);
    }

    #[test]
    fn off_mode_never_suggests_files() {
        let fixture = fixture("off");
        assert_eq!(text("cat REA", fixture.path(), Mode::Off), None);
        assert_eq!(text("cat ./REA", fixture.path(), Mode::Off), None);
    }

    #[test]
    fn mode_parses_common_names() {
        assert_eq!("off".parse(), Ok(Mode::Off));
        assert_eq!("PATH".parse(), Ok(Mode::Path));
        assert_eq!("args".parse(), Ok(Mode::Args));
        assert_eq!("all".parse(), Ok(Mode::All));
        assert!("sometimes".parse::<Mode>().is_err());
    }
}
