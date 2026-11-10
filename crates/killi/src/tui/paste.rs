//! What a paste can carry besides text: the clipboard's image, for Ctrl+V and for the empty
//! paste Kil sends on Cmd+V, and the files a terminal pastes when they are dropped on it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::mention;
use crate::tools::{image_type, image_url};

/// The files a paste names when it is a drop: a terminal writes each dropped file's absolute
/// path, shell-escaped, so a paste that is nothing but existing absolute paths is one. Anything
/// else, text with a path in it included, is None.
pub fn dropped_files(text: &str) -> Option<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = mention::words(text)
        .into_iter()
        .map(PathBuf::from)
        .collect();
    (!paths.is_empty() && paths.iter().all(|p| p.is_absolute() && p.exists())).then_some(paths)
}

/// Asks for a file copied in Finder first, so its icon isn't taken for the image; then writes
/// the image as PNG to the path it is given. macOS offers PNG for images copied as TIFF too.
#[cfg(target_os = "macos")]
const CLIPBOARD_SCRIPT: &str = r#"on run argv
	try
		return "file:" & POSIX path of (the clipboard as «class furl»)
	end try
	try
		set img to the clipboard as «class PNGf»
	on error
		return "none"
	end try
	set f to open for access (POSIX file (item 1 of argv)) with write permission
	set eof f to 0
	write img to f
	close access f
	return "png"
end run"#;

/// The clipboard's image as a data URL, or why there is none to attach.
pub fn clipboard_image() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        macos_clipboard_image()
    }
    #[cfg(target_os = "linux")]
    {
        linux_clipboard_image()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("There's no image on the clipboard.".to_string())
    }
}

/// The clipboard's image through `osascript`, as PNG bytes or a Finder file.
#[cfg(target_os = "macos")]
fn macos_clipboard_image() -> Result<String, String> {
    let file = std::env::temp_dir().join(format!("killi-paste-{}.png", std::process::id()));
    let out = Command::new("/usr/bin/osascript")
        .args(CLIPBOARD_SCRIPT.lines().flat_map(|line| ["-e", line]))
        .arg(&file)
        .output()
        .map_err(|e| format!("Could not read the clipboard: {e}"))?;
    let answer = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let image = if answer == "png" {
        image_url("the clipboard's image", &file, "image/png")
    } else if let Some(path) = answer.strip_prefix("file:").map(Path::new) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        match image_type(path) {
            Some(media_type) => image_url(&name, path, media_type),
            None => Err(format!("{name} isn't an image (png, jpg, gif or webp).")),
        }
    } else {
        Err("There's no image on the clipboard.".to_string())
    };
    let _ = fs::remove_file(&file);
    image.map(|(url, _)| url)
}

/// The clipboard's image through Wayland/X11 readers; a tool that ran but gave no image
/// bytes yields to the next, and no tool at all reads as a missing clipboard.
#[cfg(target_os = "linux")]
fn linux_clipboard_image() -> Result<String, String> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let mut ran_any = false;
    for &(program, args) in paste_candidates(wayland) {
        let out = match Command::new(program).args(args).output() {
            Err(_) => continue,
            Ok(out) => out,
        };
        ran_any = true;
        if !out.status.success() || out.stdout.is_empty() {
            continue;
        }
        if media_type_from_magic(&out.stdout).is_some() {
            return image_url_from_bytes(&out.stdout);
        }
    }
    if ran_any {
        Err("There's no image on the clipboard.".to_string())
    } else {
        Err("Could not read the clipboard: no wl-paste, xclip or xsel found.".to_string())
    }
}

/// Clipboard readers to try in order: Wayland's `wl-paste` first when on Wayland, then X11's.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn paste_candidates(wayland: bool) -> &'static [(&'static str, &'static [&'static str])] {
    const WAYLAND: &[(&str, &[&str])] = &[
        ("wl-paste", &["--type", "image/png"]),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
        ("xsel", &["--clipboard", "-o"]),
    ];
    const X11: &[(&str, &[&str])] = &[
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
        ("xsel", &["--clipboard", "-o"]),
    ];
    if wayland { WAYLAND } else { X11 }
}

/// The media type and file extension the first bytes say the clipboard holds, if an image.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn media_type_from_magic(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG") {
        return Some(("image/png", "png"));
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(("image/jpeg", "jpg"));
    }
    if bytes.starts_with(b"GIF8") {
        return Some(("image/gif", "gif"));
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(("image/webp", "webp"));
    }
    None
}

/// Clipboard bytes as a data URL, through a temp file so `image_url` checks the magic.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn image_url_from_bytes(bytes: &[u8]) -> Result<String, String> {
    let Some((_, ext)) = media_type_from_magic(bytes) else {
        return Err("There's no image on the clipboard.".to_string());
    };
    let file: PathBuf = std::env::temp_dir()
        .join(format!("killi-paste-{}.{}", std::process::id(), ext));
    let path: &Path = &file;
    if let Err(e) = fs::write(path, bytes) {
        return Err(format!("Could not read the clipboard: {e}"));
    }
    let name = "the clipboard's image";
    let image = match image_type(path) {
        Some(found) => image_url(name, path, found),
        None => Err(format!("{name} isn't an image (png, jpg, gif or webp)."))
    };
    let _ = fs::remove_file(path);
    image.map(|(url, _)| url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_is_existing_absolute_paths_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("killi-drop-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a b.png"), b"x").unwrap();
        fs::write(root.join("c.rs"), b"x").unwrap();
        let a = root.join("a b.png");
        let c = root.join("c.rs");
        assert_eq!(
            dropped_files(&format!(
                "{} {} ",
                mention::escape(&a.display().to_string()),
                c.display()
            )),
            Some(vec![a.clone(), c.clone()])
        );
        assert_eq!(
            dropped_files(&format!("'{}'\n", a.display())),
            Some(vec![a.clone()])
        );
        assert_eq!(
            dropped_files(&format!("see {}", c.display())),
            None,
            "text with a path in it is text"
        );
        assert_eq!(
            dropped_files(&format!("{}", root.join("nope").display())),
            None
        );
        assert_eq!(
            dropped_files("src/main.rs"),
            None,
            "a relative path is typed, not dropped"
        );
        assert_eq!(dropped_files(""), None);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn paste_order_prefers_wayland_then_x11() {
        let names: Vec<_> = paste_candidates(true).iter().map(|c| c.0).collect();
        assert_eq!(names, ["wl-paste", "xclip", "xsel"]);
        let names: Vec<_> = paste_candidates(false).iter().map(|c| c.0).collect();
        assert_eq!(names, ["xclip", "xsel"]);
    }

    #[test]
    fn magic_finds_png_jpeg_gif_webp() {
        assert_eq!(
            media_type_from_magic(b"\x89PNG\r\n\x1a\n"),
            Some(("image/png", "png"))
        );
        assert_eq!(
            media_type_from_magic(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(("image/jpeg", "jpg"))
        );
        assert_eq!(
            media_type_from_magic(b"GIF89a"),
            Some(("image/gif", "gif"))
        );
        assert_eq!(
            media_type_from_magic(b"RIFF\x00\x00\x00\x00WEBP"),
            Some(("image/webp", "webp"))
        );
        assert_eq!(media_type_from_magic(b"hello"), None);
        assert_eq!(media_type_from_magic(b""), None);
    }

    #[test]
    fn text_bytes_are_no_image() {
        assert_eq!(
            image_url_from_bytes(b"hello"),
            Err("There's no image on the clipboard.".to_string())
        );
    }
}
