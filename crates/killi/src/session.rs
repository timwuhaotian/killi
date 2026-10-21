//! The append-only session log: every message exactly as it was sent.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::config::killi_home;

pub struct Session {
    pub id: String,
    pub log_path: PathBuf,
    /// Holds files saved for this session, such as full outputs of truncated commands.
    pub dir: PathBuf,
    file: File,
}

impl Session {
    pub fn create(cwd: &Path) -> Result<Self> {
        let project = cwd.to_string_lossy().replace('/', "-");
        let root = killi_home().join("sessions").join(project);
        fs::create_dir_all(&root).with_context(|| format!("could not create {}", root.display()))?;
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let id = format!("{secs}-{}", std::process::id());
        let log_path = root.join(format!("{id}.jsonl"));
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("could not create {}", log_path.display()))?;
        Ok(Self { dir: root.join(&id), id, log_path, file })
    }

    pub fn record(&mut self, entry: &Value) -> Result<()> {
        writeln!(self.file, "{entry}").with_context(|| format!("could not write {}", self.log_path.display()))
    }
}
