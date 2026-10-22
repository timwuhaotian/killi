//! The clipboard's image, for Ctrl+V and for the empty paste Kil sends on Cmd+V.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::tools::{image_type, image_url};

/// Asks for a file copied in Finder first, so its icon isn't taken for the image; then writes
/// the image as PNG to the path it is given. macOS offers PNG for images copied as TIFF too.
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
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
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
