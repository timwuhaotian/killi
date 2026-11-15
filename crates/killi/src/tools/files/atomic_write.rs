use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
#[cfg(target_os = "macos")]
use std::os::macos::fs::MetadataExt as MacMetadataExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
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
    #[cfg(target_os = "macos")]
    let size = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0, 0) };
    #[cfg(target_os = "linux")]
    let size = unsafe { libc::flistxattr(fd, std::ptr::null_mut(), 0) };
    if size < 0 {
        return Err(io::Error::last_os_error());
    }
    if size > 0 {
        #[cfg(target_os = "linux")]
        return Err(unsupported(
            "cannot safely replace a file with extended attributes or ACLs",
        ));
        #[cfg(target_os = "macos")]
        {
            let mut names = vec![0u8; size as usize];
            let got = unsafe { libc::flistxattr(fd, names.as_mut_ptr().cast(), names.len(), 0) };
            if got < 0 {
                return Err(io::Error::last_os_error());
            }
            names.truncate(got as usize);
            if names
                .split(|&b| b == 0)
                .any(|name| !name.is_empty() && name != b"com.apple.provenance")
            {
                return Err(unsupported(
                    "cannot safely replace a file with extended attributes or ACLs",
                ));
            }
        }
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

#[cfg(target_os = "macos")]
fn provenance(file: &File) -> io::Result<Option<Vec<u8>>> {
    let name = c"com.apple.provenance";
    let fd = file.as_raw_fd();
    let size = unsafe { libc::fgetxattr(fd, name.as_ptr(), std::ptr::null_mut(), 0, 0, 0) };
    if size < 0 {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ENOATTR) {
            Ok(None)
        } else {
            Err(e)
        };
    }
    let mut value = vec![0; size as usize];
    let got = unsafe {
        libc::fgetxattr(
            fd,
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    value.truncate(got as usize);
    Ok(Some(value))
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
        #[cfg(target_os = "macos")]
        if provenance(original)? != provenance(&file)? {
            return Err(unsupported("cannot preserve file provenance"));
        }
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
