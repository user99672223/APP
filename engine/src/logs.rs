//! Structured logs go through `tracing` to a file in the data directory (and to
//! stderr when asked). The file is what "export logs" hands to the user.

use parking_lot::Mutex;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;
pub const LOG_FILE_NAME: &str = "engine.log";
pub const OLD_LOG_FILE_NAME: &str = "engine.log.1";

#[derive(Clone)]
struct LogFile {
    inner: Arc<Mutex<LogFileInner>>,
}

struct LogFileInner {
    path: PathBuf,
    file: File,
    written: u64,
}

impl LogFile {
    fn open(dir: &Path) -> std::io::Result<Self> {
        let path = dir.join(LOG_FILE_NAME);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            inner: Arc::new(Mutex::new(LogFileInner {
                path,
                file,
                written,
            })),
        })
    }
}

impl LogFileInner {
    /// Keep at most two files so a long session cannot fill the disk.
    fn rotate_if_needed(&mut self) {
        if self.written < MAX_LOG_BYTES {
            return;
        }
        let old = self.path.with_file_name(OLD_LOG_FILE_NAME);
        let _ = std::fs::rename(&self.path, &old);
        if let Ok(file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            self.file = file;
            self.written = 0;
        }
    }
}

impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut inner = self.inner.lock();
        inner.rotate_if_needed();
        let n = inner.file.write(buf)?;
        inner.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.lock().file.flush()
    }
}

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = LogFile;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Install the global subscriber once. Later calls (a second engine in the same
/// process, tests) keep the first subscriber and just return the log path.
pub fn init(data_dir: &Path, to_stderr: bool) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(data_dir)?;
    let log_file = LogFile::open(data_dir)?;
    let path = data_dir.join(LOG_FILE_NAME);
    let filter = EnvFilter::try_from_env("APP_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,engine=debug,iroh=warn,noq=warn"));
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(log_file);
    let registry = tracing_subscriber::registry().with(filter).with(file_layer);
    let result = if to_stderr {
        registry
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .try_init()
    } else {
        registry.try_init()
    };
    if let Err(e) = result {
        tracing::debug!("tracing already initialised: {e}");
    }
    Ok(path)
}

/// Copy the current log (and the rotated one) into a single export file and
/// return its path, so the UI can hand it to a share sheet.
pub fn export(data_dir: &Path) -> std::io::Result<PathBuf> {
    let out = data_dir.join("engine-export.log");
    let mut dst = File::create(&out)?;
    for name in [OLD_LOG_FILE_NAME, LOG_FILE_NAME] {
        if let Ok(mut src) = File::open(data_dir.join(name)) {
            std::io::copy(&mut src, &mut dst)?;
        }
    }
    dst.flush()?;
    Ok(out)
}
