use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
#[cfg(target_os = "macos")]
use std::os::macos::fs::MetadataExt as MacMetadataExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The extended attribute the system puts on ordinary files. It is allowed, and the
/// replacement must end up with the original's value (Linux copies it; macOS only compares).
#[cfg(target_os = "linux")]
const LABEL: &CStr = c"security.selinux";
#[cfg(target_os = "macos")]
const LABEL: &CStr = c"com.apple.provenance";
#[cfg(target_os = "linux")]
const NO_ATTR: i32 = libc::ENODATA;
#[cfg(target_os = "macos")]
const NO_ATTR: i32 = libc::ENOATTR;

fn unsupported(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message.into())
}

fn xattr_names(fd: libc::c_int) -> io::Result<Vec<u8>> {
    #[cfg(target_os = "macos")]
    let list = |buf: *mut libc::c_char, len| unsafe { libc::flistxattr(fd, buf, len, 0) };
    #[cfg(target_os = "linux")]
    let list = |buf: *mut libc::c_char, len| unsafe { libc::flistxattr(fd, buf, len) };
    let size = list(std::ptr::null_mut(), 0);
    if size < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut names = vec![0u8; size as usize];
    if size > 0 {
        let got = list(names.as_mut_ptr().cast(), names.len());
        if got < 0 {
            return Err(io::Error::last_os_error());
        }
        names.truncate(got as usize);
    }
    Ok(names)
}

/// The first attribute in a NUL-separated name list that a replacement would lose.
fn foreign_xattr(names: &[u8]) -> Option<&[u8]> {
    names
        .split(|&b| b == 0)
        .find(|name| !name.is_empty() && *name != LABEL.to_bytes())
}

// Resolve the final symlink ourselves so a dangling link is preserved too.
fn target(path: &Path) -> io::Result<PathBuf> {
    let mut path = path.to_path_buf();
    for _ in 0..40 {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?;
        path = fs::canonicalize(parent)?.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = fs::read_link(&path)?;
                path = if link.is_absolute() {
                    link
                } else {
                    path.parent().unwrap().join(link)
                };
            }
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(path),
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "too many symbolic links",
    ))
}

fn check_metadata(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(unsupported("not a regular file"));
    }
    if meta.nlink() != 1 {
        return Err(unsupported("cannot safely replace a file with hard links"));
    }
    let fd = file.as_raw_fd();
    if let Some(name) = foreign_xattr(&xattr_names(fd)?) {
        return Err(unsupported(format!(
            "cannot safely replace a file with extended attribute {}",
            String::from_utf8_lossy(name)
        )));
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::c_void;
        unsafe extern "C" {
            fn acl_get_fd(fd: libc::c_int) -> *mut c_void;
            fn acl_get_entry(
                acl: *mut c_void,
                id: libc::c_int,
                entry: *mut *mut c_void,
            ) -> libc::c_int;
            fn acl_free(obj: *mut c_void) -> libc::c_int;
        }
        let acl = unsafe { acl_get_fd(fd) };
        if acl.is_null() {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ENOENT) {
                return Err(e);
            }
        } else {
            let mut entry = std::ptr::null_mut();
            let result = unsafe { acl_get_entry(acl, 0, &mut entry) };
            let error = io::Error::last_os_error();
            unsafe {
                acl_free(acl);
            }
            if result < 0 {
                return Err(error);
            }
            if result > 0 {
                return Err(unsupported("cannot safely replace a file with an ACL"));
            }
        }
        if meta.st_flags() != 0 {
            return Err(unsupported("cannot safely replace a file with flags"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        let mut flags: libc::c_long = 0;
        // FS_IOC_GETFLAGS; the extent layout flag is not user metadata.
        if unsafe { libc::ioctl(fd, 0x80086601, &mut flags) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & !0x80000 != 0 {
            return Err(unsupported(
                "cannot safely replace a file with filesystem flags",
            ));
        }
    }
    Ok(())
}

/// The value of [`LABEL`] on `file`, or `None` when it has none.
fn label(file: &File) -> io::Result<Option<Vec<u8>>> {
    let fd = file.as_raw_fd();
    #[cfg(target_os = "macos")]
    let get = |buf: *mut libc::c_void, len| unsafe {
        libc::fgetxattr(fd, LABEL.as_ptr(), buf, len, 0, 0)
    };
    #[cfg(target_os = "linux")]
    let get =
        |buf: *mut libc::c_void, len| unsafe { libc::fgetxattr(fd, LABEL.as_ptr(), buf, len) };
    let size = get(std::ptr::null_mut(), 0);
    if size < 0 {
        let e = io::Error::last_os_error();
        return match e.raw_os_error() {
            Some(code) if code == NO_ATTR || code == libc::ENOTSUP => Ok(None),
            _ => Err(e),
        };
    }
    let mut value = vec![0; size as usize];
    let got = get(value.as_mut_ptr().cast(), value.len());
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    value.truncate(got as usize);
    Ok(Some(value))
}

/// Gives `temp` the original's [`LABEL`], where the system lets a file's owner set it.
fn keep_label(original: &File, temp: &File) -> io::Result<()> {
    let want = label(original)?;
    if want == label(temp)? {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if let Some(value) = &want {
        // A refusal (no SELinux, no relabel permission) is reported by the check below.
        unsafe {
            libc::fsetxattr(
                temp.as_raw_fd(),
                LABEL.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            );
        }
    }
    if want != label(temp)? {
        return Err(unsupported(format!(
            "cannot preserve extended attribute {}",
            LABEL.to_string_lossy()
        )));
    }
    Ok(())
}

struct Temp(Option<PathBuf>);
impl Drop for Temp {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

pub(in crate::tools) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let path = target(path)?;
    let original = match OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => {
            check_metadata(&file)?;
            Some(file)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let initial = original.as_ref().map(File::metadata).transpose()?;
    let parent = path.parent().unwrap();
    let (mut temp, mut file) = loop {
        let name = format!(
            ".killi-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let temp = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if original.is_some() { 0o600 } else { 0o666 })
            .open(&temp)
        {
            Ok(file) => break (Temp(Some(temp)), file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    check_metadata(&file)?;
    if let Some(original) = &original {
        let old = original.metadata()?;
        let new = file.metadata()?;
        if old.uid() != new.uid() || old.gid() != new.gid() {
            return Err(unsupported("cannot preserve file owner and group"));
        }
        keep_label(original, &file)?;
    }
    file.write_all(bytes)?;
    if let Some(original) = original {
        let mode = original.metadata()?.mode() & 0o7777;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        if file.metadata()?.mode() & 0o7777 != mode {
            return Err(unsupported("cannot preserve file permissions"));
        }
        check_metadata(&original)?;
        let old = original.metadata()?;
        let initial = initial.as_ref().unwrap();
        let current = fs::symlink_metadata(&path)?;
        if initial.ctime() != old.ctime()
            || initial.ctime_nsec() != old.ctime_nsec()
            || old.dev() != current.dev()
            || old.ino() != current.ino()
            || old.ctime() != current.ctime()
            || old.ctime_nsec() != current.ctime_nsec()
        {
            return Err(io::Error::other("file changed during write"));
        }
        fs::rename(temp.0.as_ref().unwrap(), &path)?;
        temp.0 = None;
    } else {
        fs::hard_link(temp.0.as_ref().unwrap(), &path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_system_label_is_allowed() {
        let label = LABEL.to_bytes();
        assert_eq!(foreign_xattr(b""), None);
        assert_eq!(foreign_xattr(&[label, b"\0"].concat()), None);
        assert_eq!(
            foreign_xattr(&[label, b"\0user.x\0"].concat()),
            Some(&b"user.x"[..])
        );
        assert_eq!(
            foreign_xattr(b"system.posix_acl_access\0"),
            Some(&b"system.posix_acl_access"[..])
        );
    }

    #[test]
    fn replacing_a_file_keeps_its_system_label() {
        let dir = crate::test_temp::TempDir::new("killi-write-label");
        let path = dir.join("file");
        fs::write(&path, "original").unwrap();
        let file = File::open(&path).unwrap();
        #[cfg(target_os = "linux")]
        if label(&file).unwrap().is_none() {
            // Without SELinux only a privileged process (root in CI or a container) can set it.
            let value = b"system_u:object_r:user_home_t:s0\0";
            unsafe {
                libc::fsetxattr(
                    file.as_raw_fd(),
                    LABEL.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                );
            }
        }
        let Some(before) = label(&file).unwrap() else {
            eprintln!("skipped: cannot give a file {LABEL:?} here");
            return;
        };
        write(&path, b"changed").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"changed");
        assert_eq!(label(&File::open(&path).unwrap()).unwrap(), Some(before));
        write(&dir.join("new"), b"new").unwrap();
        assert_eq!(fs::read(dir.join("new")).unwrap(), b"new");
    }
}
