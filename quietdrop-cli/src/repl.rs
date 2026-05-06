//! Interactive line editor with persistent **command** history.
//!
//! The CLI prompt accepts two kinds of input:
//!   1. Slash-commands (e.g. `/help`, `/recipient Bob`) — these are saved to
//!      history so Up/Down/Ctrl+R can recall them across sessions, just like
//!      a shell.
//!   2. Free-form message bodies — these are encrypted and sent over the
//!      wire and are deliberately **never** persisted. Recall would mostly
//!      be useless and the on-disk file would otherwise become a plaintext
//!      transcript.
//!
//! Wraps [`rustyline`] so the rest of the CLI gets:
//! - Up / Down arrow history navigation (commands only)
//! - Left / Right arrow + Home / End line editing
//! - Ctrl+R reverse history search
//! - Ctrl+A / E / U / K / W standard shortcuts
//! - History persisted to a per-user data dir between sessions

use std::path::{Path, PathBuf};

use rustyline::config::Configurer;
use rustyline::error::ReadlineError;
use rustyline::history::History;
use rustyline::{Config, DefaultEditor};

const HISTORY_FILE: &str = "history.txt";
const APP_DIR: &str = "quietdrop";
const MAX_HISTORY: usize = 1000;

/// Outcome of a single prompt read.
pub enum ReadOutcome {
    /// User entered a (possibly empty) line.
    Line(String),
    /// User pressed Ctrl+C.
    Interrupted,
    /// User pressed Ctrl+D on an empty line, or stdin closed.
    Eof,
}

pub struct Repl {
    editor: DefaultEditor,
    history_path: PathBuf,
    persist: bool,
}

impl Repl {
    /// Create a new REPL, loading any existing history file.
    ///
    /// `persist == false` disables both loading and saving the on-disk
    /// history file (useful for `--no-history` / ephemeral sessions).
    pub fn new(persist: bool) -> rustyline::Result<Self> {
        let config = Config::builder()
            .auto_add_history(false) // we add only commands, manually
            .max_history_size(MAX_HISTORY)?
            .history_ignore_dups(true)?
            .history_ignore_space(true)
            .build();

        let mut editor = DefaultEditor::with_config(config)?;
        editor.set_max_history_size(MAX_HISTORY)?;
        editor.set_history_ignore_dups(true)?;

        let history_path = default_history_path();

        if persist {
            if let Some(parent) = history_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if history_path.exists() {
                let _ = editor.load_history(&history_path);
            }
        }

        Ok(Self {
            editor,
            history_path,
            persist,
        })
    }

    /// Read one line of free-form input. The line is **not** added to
    /// history — used for message bodies and other non-command input.
    pub fn readline(&mut self, prompt: &str) -> ReadOutcome {
        self.read_raw(prompt)
    }

    /// Manually record a command in the history. Call this from the CLI
    /// after detecting that a line is a slash-command.
    ///
    /// Sensitive commands (see [`is_sensitive`]) are silently dropped.
    pub fn record_command(&mut self, line: &str) {
        if line.starts_with('/') && !is_sensitive(line) {
            let _ = self.editor.add_history_entry(line);
        }
    }

    fn read_raw(&mut self, prompt: &str) -> ReadOutcome {
        match self.editor.readline(prompt) {
            Ok(line) => ReadOutcome::Line(line.trim().to_owned()),
            Err(ReadlineError::Interrupted) => ReadOutcome::Interrupted,
            Err(ReadlineError::Eof) => ReadOutcome::Eof,
            Err(err) => {
                eprintln!("readline error: {err}");
                ReadOutcome::Eof
            }
        }
    }

    /// Snapshot of the history as owned strings (oldest first).
    pub fn history_snapshot(&self) -> Vec<String> {
        let h = self.editor.history();
        let mut out = Vec::with_capacity(h.len());
        for i in 0..h.len() {
            if let Ok(Some(sr)) = h.get(i, rustyline::history::SearchDirection::Forward) {
                out.push(sr.entry.into_owned());
            }
        }
        out
    }

    /// Clear in-memory history and remove the on-disk file.
    pub fn clear_history(&mut self) {
        let _ = self.editor.clear_history();
        let _ = std::fs::remove_file(&self.history_path);
    }

    /// Path to the persistent history file.
    pub fn history_path(&self) -> &Path {
        &self.history_path
    }

    /// Whether history is being persisted to disk this session.
    pub fn is_persistent(&self) -> bool {
        self.persist
    }

    /// Persist history to disk. Called automatically on drop.
    pub fn save(&mut self) {
        if !self.persist {
            return;
        }
        if let Some(parent) = self.history_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = self.editor.save_history(&self.history_path);
        tighten_permissions(&self.history_path);
    }
}

impl Drop for Repl {
    fn drop(&mut self) {
        self.save();
    }
}

fn default_history_path() -> PathBuf {
    dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_DIR)
        .join(HISTORY_FILE)
}

/// Belt-and-braces filter so obvious secret-bearing commands aren't persisted
/// even if a user types them at the command prompt.
fn is_sensitive(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.starts_with("/password ")
        || lower.starts_with("/passwd ")
        || lower.starts_with("/login ")
        || lower.starts_with("/auth ")
        || lower.starts_with("/token ")
}

#[cfg(unix)]
fn tighten_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn tighten_permissions(_path: &Path) {
    // On Windows the file lives under %LOCALAPPDATA%\quietdrop\, which is
    // already user-scoped via the standard NTFS ACLs inherited from the
    // user profile directory. Nothing extra to do.
}
