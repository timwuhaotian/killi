//! `killi update`: replaces this binary with the published one after checking its SHA-256.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Where releases are published; `KILLI_RELEASES_URL` points elsewhere, e.g. a local mirror.
const RELEASES_URL: &str = "https://github.com/timwuhaotian/killi/raw/main/releases";
const BINARY: &str = "killi-arm64";

pub async fn update() -> Result<()> {
    let base = std::env::var("KILLI_RELEASES_URL").unwrap_or_else(|_| RELEASES_URL.to_string());
    let http = reqwest::Client::new();
    let latest = http.get(format!("{base}/latest.toml")).send().await?.error_for_status()?.text().await?;
    let latest: toml::Table = toml::from_str(&latest).context("invalid latest.toml")?;
    let (Some(version), Some(sha256)) =
        (latest.get("version").and_then(toml::Value::as_str), latest.get("sha256").and_then(toml::Value::as_str))
    else {
        bail!("latest.toml needs `version` and `sha256`");
    };
    let current = env!("CARGO_PKG_VERSION");
    if parse(version) <= parse(current) {
        println!("killi {current} is up to date.");
        return Ok(());
    }

    let exe = fs::canonicalize(std::env::current_exe()?)?;
    let staging = exe.with_file_name(format!(".killi-update-{}", std::process::id()));
    let bytes = http.get(format!("{base}/{BINARY}")).send().await?.error_for_status()?.bytes().await?;
    fs::write(&staging, &bytes).with_context(|| format!("could not write {}", staging.display()))?;
    let result = verify_and_install(&staging, sha256, &exe);
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result?;
    println!("Updated killi {current} → {version} ({}).", exe.display());
    Ok(())
}

fn verify_and_install(staging: &std::path::Path, expected: &str, exe: &std::path::Path) -> Result<()> {
    let output = Command::new("/usr/bin/shasum").arg("-a").arg("256").arg(staging).output()?;
    let actual = String::from_utf8_lossy(&output.stdout);
    let actual = actual.split_whitespace().next().unwrap_or_default();
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("checksum mismatch: expected {expected}, downloaded {actual}");
    }
    fs::set_permissions(staging, fs::Permissions::from_mode(0o755))?;
    fs::rename(staging, exe).with_context(|| format!("could not replace {}", exe.display()))
}

fn parse(version: &str) -> Vec<u64> {
    version.split('.').map(|part| part.parse().unwrap_or(0)).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_compare_numerically() {
        assert!(super::parse("0.10.0") > super::parse("0.9.3"));
        assert!(super::parse("0.1.0") == super::parse("0.1.0"));
    }
}
