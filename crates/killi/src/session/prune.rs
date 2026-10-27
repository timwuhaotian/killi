//! Deleting old sessions: every log but the newest few, with its saved files.

use super::*;

/// Deletes every session but the `keep` newest and `current`, each with its side folder, and
/// the project folders left empty.
pub fn prune(keep: usize, current: &Path) -> Result<Pruned> {
    prune_in(&killi_home().join("sessions"), keep, current)
}

pub(super) fn prune_in(root: &Path, keep: usize, current: &Path) -> Result<Pruned> {
    let mut pruned = Pruned::default();
    let live = live_logs();
    let old: Vec<PathBuf> = list_all_in(root).into_iter().map(|(path, _)| path).filter(|path| deletable(path, current, &live)).skip(keep).collect();
    for log in old {
        // Checked again per log: a session may have resumed this one since the list above
        // was taken, while earlier (larger) side folders were being removed.
        if !deletable(&log, current, &live_logs()) {
            continue;
        }
        let dir = log.with_extension("");
        if dir.is_dir() {
            pruned.bytes += dir_size(&dir);
            fs::remove_dir_all(&dir).with_context(|| format!("could not remove {}", dir.display()))?;
        }
        pruned.bytes += fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
        fs::remove_file(&log).with_context(|| format!("could not remove {}", log.display()))?;
        pruned.sessions += 1;
        if let Some(project) = log.parent()
            && fs::read_dir(project).is_ok_and(|mut entries| entries.next().is_none())
        {
            let _ = fs::remove_dir(project);
        }
    }
    Ok(pruned)
}

/// Whether `prune` may delete this log: not the session running now, not one that is still
/// running — each live session locks its log and lists it in `~/.killi/live` — and not one a
/// live session lists.
pub(super) fn deletable(path: &Path, current: &Path, live: &HashSet<PathBuf>) -> bool {
    path != current && !live.contains(path) && !is_locked(path)
}

/// The logs live sessions list as theirs in `~/.killi/live` (`live::Entry::log`).
fn live_logs() -> HashSet<PathBuf> {
    crate::live::list().into_iter().map(|entry| entry.log).collect()
}

fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() { dir_size(&path) } else { fs::metadata(&path).map(|m| m.len()).unwrap_or(0) }
        })
        .sum()
}
