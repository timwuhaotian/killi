use std::fs;

use serde_json::Value;

use super::{Context, MAX_BYTES, MAX_LINES, Output, is_binary, resolve};
use crate::llm::truncate;

pub(super) mod atomic_write;
mod image;

pub(super) use image::read_image;
pub use image::{image_type, image_url};

const MAX_LINE_CHARS: usize = 2000;
/// Larger text files are not read whole: the file, its decoded copy and its lines would take
/// several times its size in memory. `bash` can read part of one (`sed -n`, `tail`).
const MAX_READ: u64 = 10 * 1024 * 1024;

pub async fn read(ctx: &Context, args: &Value) -> Output {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return Output::error("read needs `path`.");
    };
    let full = resolve(ctx, path);
    if full.is_dir() {
        return Output::error(format!("{path} is a directory; use glob to list it."));
    }
    if let Some(media_type) = image_type(&full) {
        return read_image(ctx, path, &full, media_type).await;
    }
    if let Ok(meta) = fs::metadata(&full)
        && meta.len() > MAX_READ
    {
        return Output::error(format!(
            "{path} is {} MB, too large to read whole. Search or read part of it with bash (`rg`/`grep`, `sed -n '1000,1100p'`, `head`, `tail`).",
            meta.len() / (1024 * 1024)
        ));
    }
    let bytes = match fs::read(&full) {
        Ok(bytes) => bytes,
        Err(e) => return Output::error(format!("Could not read {path}: {e}")),
    };
    if is_binary(&bytes) {
        return Output::error(format!(
            "{path} is a binary file ({} bytes); not shown.",
            bytes.len()
        ));
    }
    let text = TextFile::decode(bytes).text;
    if text.is_empty() {
        return Output::ok("(empty file)");
    }

    let lines = || text.split_terminator('\n');
    let total = lines().count();
    let offset = super::uint(args, "offset").unwrap_or(1).max(1) as usize;
    if offset > total {
        return Output::error(format!(
            "Offset {offset} is beyond the end of {path} ({total} lines)."
        ));
    }
    let limit = super::uint(args, "limit").map_or(MAX_LINES, |l| (l as usize).clamp(1, MAX_LINES));

    let mut out = String::new();
    let mut entry = String::new();
    let mut last = offset - 1;
    let mut hit_bytes = false;
    for (i, line) in lines().enumerate().skip(offset - 1).take(limit) {
        entry.clear();
        if line.len() > MAX_LINE_CHARS {
            use std::fmt::Write;
            let _ = writeln!(
                entry,
                "{}\t{}… [line truncated: {} chars]",
                i + 1,
                truncate(line, MAX_LINE_CHARS),
                line.len()
            );
        } else {
            use std::fmt::Write;
            let _ = writeln!(entry, "{}\t{line}", i + 1);
        }
        if !out.is_empty() && out.len() + entry.len() > MAX_BYTES {
            hit_bytes = true;
            break;
        }
        out.push_str(&entry);
        last = i + 1;
    }
    let brief = match (offset, total) {
        (1, 1) => "1 line".to_string(),
        (1, _) if last == total => format!("{total} lines"),
        _ => format!("lines {offset}-{last} of {total}"),
    };
    if last < total {
        let why = if hit_bytes { " (50KB limit)" } else { "" };
        out.push_str(&format!(
            "[Showing lines {offset}-{last} of {total}{why}. Use offset={} to continue.]",
            last + 1
        ));
    }
    Output::ok(out).brief(brief)
}

pub fn write(ctx: &Context, args: &Value) -> Output {
    let (Some(path), Some(content)) = (
        args.get("path").and_then(Value::as_str),
        args.get("content").and_then(Value::as_str),
    ) else {
        return Output::error("write needs `path` and `content`.");
    };
    let full = resolve(ctx, path);
    let existed = full.exists();
    if let Some(parent) = full.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        return Output::error(format!("Could not create {}: {e}", parent.display()));
    }
    if let Err(e) = atomic_write::write(&full, content.as_bytes()) {
        return Output::error(format!("Could not write {path}: {e}"));
    }
    let verb = if existed { "Overwrote" } else { "Created" };
    let lines = content.lines().count();
    Output::ok(format!("{verb} {path} ({}).", super::count(lines, "line"))).brief(format!(
        "{} {}",
        verb.to_lowercase(),
        super::count(lines, "line")
    ))
}

/// A text file as read and edit see it: BOM removed, and CRLF shown as LF when every line
/// ends in CRLF, so both tools work on the same view and writes restore the original form.
pub struct TextFile {
    pub text: String,
    bom: bool,
    crlf: bool,
}

impl TextFile {
    pub fn decode(bytes: Vec<u8>) -> Self {
        let raw = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        };
        let (bom, raw) = match raw.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest.to_string()),
            None => (false, raw),
        };
        let mut newlines = 0;
        let mut all_crlf = true;
        for (i, _) in raw.match_indices('\n') {
            newlines += 1;
            if i == 0 || raw.as_bytes()[i - 1] != b'\r' {
                all_crlf = false;
            }
        }
        let crlf = newlines > 0 && all_crlf;
        let text = if crlf { raw.replace("\r\n", "\n") } else { raw };
        Self { text, bom, crlf }
    }

    pub fn encode(&self, text: &str) -> String {
        let body = if self.crlf {
            text.replace('\n', "\r\n")
        } else {
            text.to_string()
        };
        if self.bom {
            format!("\u{feff}{body}")
        } else {
            body
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;
    use std::process::Command;

    fn ctx(dir: &std::path::Path) -> Context {
        Context::new(dir.to_path_buf(), dir.join("spill"))
    }

    #[test]
    fn failed_overwrites_leave_original_intact() {
        use std::os::unix::process::CommandExt;

        if let Ok(root) = std::env::var("KILLI_WRITE_FAILURE_TEST") {
            let dir = Path::new(&root);
            let context = ctx(dir);
            for name in ["write", "edit"] {
                let out = if name == "write" {
                    write(
                        &context,
                        &json!({"path": name, "content": "replacement content"}),
                    )
                } else {
                    super::super::edit::edit(
                        &context,
                        &json!({"path": name, "old_string": "original", "new_string": "replacement content"}),
                    )
                };
                assert!(out.is_error, "{name}: {}", out.text);
            }
            return;
        }
        let dir = crate::test_temp::TempDir::new("killi-write-failure");
        for name in ["write", "edit"] {
            fs::write(dir.join(name), "original").unwrap();
        }
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "tools::files::tests::failed_overwrites_leave_original_intact",
            "--nocapture",
        ])
        .env("KILLI_WRITE_FAILURE_TEST", dir.path());
        unsafe {
            cmd.pre_exec(|| {
                let limit = libc::rlimit {
                    rlim_cur: 4,
                    rlim_max: 4,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
                Ok(())
            });
        }
        let result = cmd.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        for name in ["edit", "write"] {
            assert_eq!(fs::read(dir.join(name)).unwrap(), b"original", "{name}");
        }
    }

    #[test]
    fn writes_follow_symlinks_and_keep_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = crate::test_temp::TempDir::new("killi-write-link");
        let target = dir.join("target");
        fs::write(&target, "original").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        symlink("target", dir.join("link")).unwrap();
        let out = write(&ctx(&dir), &json!({"path": "link", "content": "new"}));
        assert!(!out.is_error, "{}", out.text);
        assert!(dir.join("link").is_symlink());
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let out = super::super::edit::edit(
            &ctx(&dir),
            &json!({"path": "link", "old_string": "new", "new_string": "edited"}),
        );
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(fs::read(&target).unwrap(), b"edited");
        assert!(dir.join("link").is_symlink());
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        symlink("missing", dir.join("dangling")).unwrap();
        let out = write(
            &ctx(&dir),
            &json!({"path": "dangling", "content": "created"}),
        );
        assert!(!out.is_error, "{}", out.text);
        assert!(dir.join("dangling").is_symlink());
        assert_eq!(fs::read(dir.join("missing")).unwrap(), b"created");
    }

    #[test]
    fn writes_reject_hard_links_and_extended_attributes() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let dir = crate::test_temp::TempDir::new("killi-write-metadata");
        let context = ctx(&dir);
        let path = dir.join("file");
        fs::write(&path, "original").unwrap();
        fs::hard_link(&path, dir.join("other")).unwrap();
        let attempt = || write(&context, &json!({"path": "file", "content": "changed"}));
        assert!(attempt().is_error);
        let edit = || {
            super::super::edit::edit(
                &context,
                &json!({"path": "file", "old_string": "original", "new_string": "changed"}),
            )
        };
        assert!(edit().is_error);
        assert_eq!(fs::read(dir.join("other")).unwrap(), b"original");
        fs::remove_file(dir.join("other")).unwrap();
        let name = CString::new(if cfg!(target_os = "macos") {
            "org.killi.test"
        } else {
            "user.killi_test"
        })
        .unwrap();
        let path_c = CString::new(path.as_os_str().as_bytes()).unwrap();
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::setxattr(
                path_c.as_ptr(),
                name.as_ptr(),
                b"value".as_ptr().cast(),
                5,
                0,
                0,
            )
        };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::setxattr(
                path_c.as_ptr(),
                name.as_ptr(),
                b"value".as_ptr().cast(),
                5,
                0,
            )
        };
        assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
        let out = attempt();
        assert!(out.is_error);
        assert!(out.text.contains(name.to_str().unwrap()), "{}", out.text);
        assert!(edit().is_error);
        assert_eq!(fs::read(&path).unwrap(), b"original");
    }

    #[test]
    fn new_files_obey_umask() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::process::CommandExt;
        if let Ok(root) = std::env::var("KILLI_WRITE_UMASK_TEST") {
            let dir = Path::new(&root);
            let out = write(&ctx(dir), &json!({"path": "new", "content": "new"}));
            assert!(!out.is_error, "{}", out.text);
            assert_eq!(
                fs::metadata(dir.join("new")).unwrap().permissions().mode() & 0o777,
                0o640
            );
            return;
        }
        let dir = crate::test_temp::TempDir::new("killi-write-umask");
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "tools::files::tests::new_files_obey_umask"])
            .env("KILLI_WRITE_UMASK_TEST", dir.path());
        unsafe {
            cmd.pre_exec(|| {
                libc::umask(0o137);
                Ok(())
            });
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_numbers_lines_and_points_to_the_rest() {
        let dir = crate::test_temp::TempDir::new("killi-read");
        let body: String = (1..=5).map(|n| format!("line {n}\r\n")).collect();
        fs::write(dir.join("a.txt"), body).unwrap();
        let out = read(
            &ctx(&dir),
            &json!({"path": "a.txt", "offset": 2, "limit": 2}),
        )
        .await;
        assert_eq!(
            out.text,
            "2\tline 2\n3\tline 3\n[Showing lines 2-3 of 5. Use offset=4 to continue.]"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn offset_and_limit_arrive_as_strings_too() {
        let dir = crate::test_temp::TempDir::new("killi-read-str");
        fs::write(dir.join("a.txt"), "a\nb\nc\n").unwrap();
        let out = read(
            &ctx(&dir),
            &json!({"path": "a.txt", "offset": "2", "limit": "1"}),
        )
        .await;
        assert_eq!(
            out.text,
            "2\tb\n[Showing lines 2-2 of 3. Use offset=3 to continue.]"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_file_too_large_to_read_whole_is_not_read() {
        let dir = crate::test_temp::TempDir::new("killi-bigread");
        let file = fs::File::create(dir.join("big.log")).unwrap();
        file.set_len(MAX_READ + 1).unwrap();
        let out = read(&ctx(&dir), &json!({"path": "big.log"})).await;
        assert!(
            out.is_error && out.text.contains("too large to read whole"),
            "{}",
            out.text
        );
        fs::write(dir.join("ok.txt"), "a\nb\n").unwrap();
        assert_eq!(
            read(&ctx(&dir), &json!({"path": "ok.txt", "offset": 2}))
                .await
                .text,
            "2\tb\n"
        );
        assert!(
            read(&ctx(&dir), &json!({"path": "ok.txt", "offset": 3}))
                .await
                .is_error
        );
    }

    #[test]
    fn crlf_and_bom_round_trip() {
        let file = TextFile::decode("\u{feff}a\r\nb\r\n".as_bytes().to_vec());
        assert_eq!(file.text, "a\nb\n");
        assert_eq!(file.encode("a\nc\n"), "\u{feff}a\r\nc\r\n");
    }
}
