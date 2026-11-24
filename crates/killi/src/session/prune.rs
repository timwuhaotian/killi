//! Deleting old sessions: every log but the newest few, with its saved files.

use super::*;

/// Deletes every session but the `keep` newest and `current`, each with its side folder, and
/// the project folders left empty. Undo snapshot repos no session uses are cleaned too
/// (`snapshot::prune_store`); `Pruned` counts only the sessions.
pub fn prune(keep: usize, current: &Path) -> Result<Pruned> {
    let pruned = prune_in(&killi_home().join("sessions"), keep, current);
    crate::snapshot::prune_store();
    pruned
}

pub(super) fn prune_in(root: &Path, keep: usize, current: &Path) -> Result<Pruned> {
    let mut pruned = Pruned::default();
    let live = live_logs();
    let old: Vec<PathBuf> = list_all_in(root)
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| deletable(path, current, &live))
        .skip(keep)
        .collect();
    for log in old {
        // Checked again per log: a session may have resumed this one since the list above
        // was taken, while earlier (larger) side folders were being removed.
        let Some(bytes) = remove_unlocked(&log, current, &live_logs())? else {
            continue;
        };
        pruned.bytes += bytes;
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

/// Deletes a log and its side folder, holding the log's lock until both are gone, so a
/// session can't resume it meanwhile and go on writing to a deleted file. `None`, deleting
/// nothing, for `current`, a log a live session lists, or one whose lock is taken; otherwise
/// the bytes freed.
pub(super) fn remove_unlocked(
    log: &Path,
    current: &Path,
    live: &HashSet<PathBuf>,
) -> Result<Option<u64>> {
    if log == current || live.contains(log) {
        return Ok(None);
    }
    let Ok(held) = File::open(log) else {
        return Ok(None);
    };
    if held.try_lock().is_err() {
        return Ok(None);
    }
    let mut bytes = 0;
    let dir = log.with_extension("");
    if dir.is_dir() {
        bytes += dir_size(&dir);
        fs::remove_dir_all(&dir).with_context(|| format!("could not remove {}", dir.display()))?;
    }
    bytes += held.metadata().map(|m| m.len()).unwrap_or(0);
    fs::remove_file(log).with_context(|| format!("could not remove {}", log.display()))?;
    drop(held);
    Ok(Some(bytes))
}

/// The logs live sessions list as theirs in `~/.killi/live` (`live::Entry::log`).
fn live_logs() -> HashSet<PathBuf> {
    crate::live::list()
        .into_iter()
        .map(|entry| entry.log)
        .collect()
}

fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                dir_size(&path)
            } else {
                fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}
