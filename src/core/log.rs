use anyhow::{anyhow, Result};
use log::{Level, LevelFilter, Log, Metadata, Record};
use simplelog::{Config, WriteLogger};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const LOG_FILE: &str = "ActiveDesktop.log";

pub fn init(log_path: &Path) -> Result<()> {
    let file = File::create(log_path)?;
    WriteLogger::init(LevelFilter::Info, Config::default(), file)?;
    Ok(())
}

/// Candidate log paths in priority order: next to the executable, then
/// `%LOCALAPPDATA%\ActiveDesktop\`, then the system temp directory.
pub fn candidate_log_paths() -> Vec<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let local_appdata = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("ActiveDesktop"));
    candidates_from(exe_dir, local_appdata, std::env::temp_dir())
}

fn candidates_from(
    exe_dir: Option<PathBuf>,
    local_appdata: Option<PathBuf>,
    temp: PathBuf,
) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(3);
    if let Some(dir) = exe_dir {
        if !dir.as_os_str().is_empty() {
            out.push(dir.join(LOG_FILE));
        }
    }
    if let Some(dir) = local_appdata {
        out.push(dir.join(LOG_FILE));
    }
    out.push(temp.join(LOG_FILE));
    out
}

/// Initialize logging, trying each candidate path until one is writable.
///
/// Logging is best-effort: callers must treat an error as a warning and keep
/// running. A failure to open the log must never abort the wallpaper, so do not
/// call `process::exit` from here or from the caller's error branch.
type LogBuffer = Arc<Mutex<Vec<String>>>;

/// Where logs go, and where they must be flushed if the run ends abnormally.
#[derive(Clone)]
pub struct LogSink {
    buffer: Option<LogBuffer>,
    path: Option<PathBuf>,
}

impl LogSink {
    fn file_sink(path: PathBuf) -> Self {
        Self {
            buffer: None,
            path: Some(path),
        }
    }
    fn buffer_sink(buffer: LogBuffer) -> Self {
        Self {
            buffer: Some(buffer),
            path: None,
        }
    }
    pub fn disabled() -> Self {
        Self {
            buffer: None,
            path: None,
        }
    }
    pub fn location(&self) -> String {
        match (&self.path, &self.buffer) {
            (Some(path), _) => path.display().to_string(),
            (None, Some(_)) => "memory buffer (release; written only on failure)".to_string(),
            (None, None) => "stderr".to_string(),
        }
    }
    /// Release: write buffered logs to disk. Debug: already written live.
    pub fn persist_failure(&self) {
        let Some(buffer) = &self.buffer else { return };
        let lines = match buffer.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        if lines.is_empty() {
            return;
        }
        if let Err(e) = write_lines(&lines, candidate_log_paths()) {
            eprintln!("warning: failed to persist failure log: {e:#}");
        }
    }
}

struct BufferLogger {
    buffer: LogBuffer,
}

impl Log for BufferLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }
    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!(
            "{secs} [{}] {}
",
            record.level(),
            record.args()
        );
        if let Ok(mut buffer) = self.buffer.lock() {
            buffer.push(line);
        }
    }
    fn flush(&self) {}
}

fn write_lines(lines: &[String], paths: Vec<PathBuf>) -> Result<PathBuf> {
    let mut failures: Vec<String> = Vec::new();
    for path in paths {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    failures.push(format!("{}: create dir: {e}", path.display()));
                    continue;
                }
            }
        }
        match std::fs::write(&path, lines.concat()) {
            Ok(()) => return Ok(path),
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }
    Err(anyhow!("no writable log path: {}", failures.join("; ")))
}

/// Initialize logging. Debug builds log to a file immediately; release builds
/// buffer logs in memory and only persist them if the run ends abnormally.
pub fn init_preferred() -> Result<LogSink> {
    if cfg!(debug_assertions) {
        let path = init_candidates(candidate_log_paths())?;
        Ok(LogSink::file_sink(path))
    } else {
        let buffer: LogBuffer = Arc::new(Mutex::new(Vec::new()));
        log::set_boxed_logger(Box::new(BufferLogger {
            buffer: buffer.clone(),
        }))
        .map_err(|e| anyhow!("set logger: {e}"))?;
        log::set_max_level(LevelFilter::Info);
        Ok(LogSink::buffer_sink(buffer))
    }
}

fn init_candidates(paths: Vec<PathBuf>) -> Result<PathBuf> {
    let mut failures: Vec<String> = Vec::new();
    for path in paths {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    failures.push(format!("{}: create dir: {e}", path.display()));
                    continue;
                }
            }
        }
        match init(&path) {
            Ok(()) => return Ok(path),
            Err(e) => failures.push(format!("{}: {e:#}", path.display())),
        }
    }
    Err(anyhow!(
        "no writable log path ({} candidate(s) tried): {}",
        failures.len(),
        failures.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_prefer_exe_then_localappdata_then_temp() {
        let paths = candidates_from(
            Some(PathBuf::from(r"C:\apps\ActiveDesktop")),
            Some(PathBuf::from(r"C:\Users\me\AppData\Local\ActiveDesktop")),
            PathBuf::from(r"C:\Windows\Temp"),
        );
        assert_eq!(
            paths,
            vec![
                PathBuf::from(r"C:\apps\ActiveDesktop\ActiveDesktop.log"),
                PathBuf::from(r"C:\Users\me\AppData\Local\ActiveDesktop\ActiveDesktop.log"),
                PathBuf::from(r"C:\Windows\Temp\ActiveDesktop.log"),
            ]
        );
    }

    #[test]
    fn missing_exe_dir_is_skipped() {
        let paths = candidates_from(None, None, PathBuf::from(r"C:\Temp"));
        assert_eq!(paths, vec![PathBuf::from(r"C:\Temp\ActiveDesktop.log")]);
    }

    #[test]
    fn write_lines_falls_back_to_writable_path() {
        let dir = std::env::temp_dir().join(format!(
            "active_desktop_persist_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let blocker = dir.join("blocker");
        std::fs::create_dir_all(&blocker).unwrap();
        let bad = blocker.clone();
        let good = dir.join("out.log");
        let lines = vec![
            "a
"
            .to_string(),
            "b
"
            .to_string(),
        ];
        let chosen = write_lines(&lines, vec![bad, good.clone()]).expect("fallback path");
        assert_eq!(chosen, good);
        assert_eq!(
            std::fs::read_to_string(&good).unwrap(),
            "a
b
"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unwritable_candidate_falls_through_to_next() {
        let dir =
            std::env::temp_dir().join(format!("active_desktop_log_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        // A regular file used as a directory makes File::create under it fail,
        // exercising the fallback without needing an ACL-restricted folder.
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let bad = blocker.join("ActiveDesktop.log");
        let good = dir.join("fallback.log");
        let chosen = init_candidates(vec![bad, good.clone()]).expect("fallback log path");
        assert_eq!(chosen, good);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
