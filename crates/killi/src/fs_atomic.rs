use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result};

/// Writes `bytes` to `path` whole, owner-only by default, through a temporary file in the
/// same directory that is synced and renamed into place, so a concurrent reader or writer
/// never sees half of it. `Some(mode)` is the mode the new file gets (`0o600` for a key);
/// `None` keeps the mode the file has, falling back to `0o600` for a file that doesn't
/// exist yet.
pub fn write(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    // A symlink at `path` is written through, as a plain write would: the rename replaces
    // its target, not the link.
    let dest = if path.is_symlink() {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    let mode = match mode {
        Some(mode) => mode,
        None => fs::metadata(&dest).map_or(0o600, |meta| meta.permissions().mode() & 0o7777),
    };
    let name = dest.file_name().map_or_else(
        || "file".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temporary = dest.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&temporary)?;
        // `mode` only applies when the temporary is created; a stale one left by a failed
        // write keeps its old mode otherwise.
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &dest)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written.with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn write_replaces_whole_and_applies_or_keeps_the_mode() {
        let root = std::env::temp_dir().join(format!("killi-fs-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("data");
        std::fs::write(&path, b"old").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&path, permissions).unwrap();

        super::write(&path, b"new", None).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644,
            "a None mode keeps the file's"
        );

        super::write(&path, b"keyed", Some(0o600)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"keyed");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let fresh = root.join("made");
        super::write(&fresh, b"first", None).unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"first");
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o600,
            "a new file is owner-only"
        );

        let stale = root.join(format!(".freshkey.{}.tmp", std::process::id()));
        std::fs::write(&stale, b"stale").unwrap();
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();
        let freshkey = root.join("freshkey");
        super::write(&freshkey, b"secret", Some(0o600)).unwrap();
        assert_eq!(
            std::fs::metadata(&freshkey).unwrap().permissions().mode() & 0o777,
            0o600,
            "a stale temporary must not widen the mode"
        );

        let link = root.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        super::write(&link, b"through", None).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link stays a link"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"through",
            "the target got the write"
        );

        let names: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            names.len(),
            4,
            "the temporary files are renamed away: {names:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
