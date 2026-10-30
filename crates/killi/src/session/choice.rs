//! The model last picked, kept for the sessions started after it.

use super::*;

fn choice_path() -> PathBuf {
    killi_home().join("last-model.json")
}

/// The model last picked with /model, which new sessions start on. Its effort is not kept
/// here: each model's `default_effort` in the config holds it, and `/effort` writes it.
/// Nothing when none was picked or the file can't be read.
pub fn last_choice() -> Choice {
    read_choice(&choice_path())
}

/// Remembers a pick from /model for the sessions started after it.
pub fn save_choice(choice: &Choice) -> Result<()> {
    write_choice(&choice_path(), choice)
}

pub(super) fn read_choice(path: &Path) -> Choice {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub(super) fn write_choice(path: &Path, choice: &Choice) -> Result<()> {
    crate::fs_atomic::write(path, serde_json::to_string(choice)?.as_bytes(), None)
        .with_context(|| format!("could not write {}", path.display()))
}
