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
}
