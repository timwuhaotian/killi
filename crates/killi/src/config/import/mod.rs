//! Copies providers, models, the default model and MCP servers from other coding
//! agents' configs. Every importer ends in `write_import`; `sources` lists them for the
//! first-run offer and `killi import`.

mod claude;
mod codex;
mod gemini;
mod kimi;
mod opencode;
mod pi;

use std::fs;
use std::io::{IsTerminal, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub use claude::import_claude;
pub use codex::import_codex;
pub use gemini::import_gemini;
pub use kimi::import_kimi;
pub use opencode::import_opencode;
pub use pi::{import_omp, import_pi};
#[cfg(test)]
pub(crate) use claude::import_claude_from;
#[cfg(test)]
pub(crate) use codex::import_codex_from;
#[cfg(test)]
pub(crate) use gemini::import_gemini_from;
#[cfg(test)]
pub(crate) use opencode::import_opencode_from;
#[cfg(test)]
pub(crate) use pi::{import_omp_from, import_pi_from};

use super::*;

/// A coding agent whose config killi can import: its `killi import` id, display name, the
/// config that shows it is set up on this machine, and the importer for it.
pub struct Source {
    pub id: &'static str,
    pub name: &'static str,
    pub detect: fn() -> Option<PathBuf>,
    pub import: fn(bool) -> Result<()>,
}

/// The other coding agents whose config killi can import, in the order the first-run
/// offer and `killi import` list them.
pub fn sources() -> Vec<Source> {
    vec![
        Source {
            id: "kimi",
            name: "Kimi Code",
            detect: detect_kimi,
            import: import_kimi,
        },
        Source {
            id: "pi",
            name: "pi",
            detect: detect_pi,
            import: import_pi,
        },
        Source {
            id: "omp",
            name: "omp",
            detect: detect_omp,
            import: import_omp,
        },
        Source {
            id: "opencode",
            name: "opencode",
            detect: detect_opencode,
            import: import_opencode,
        },
        Source {
            id: "codex",
            name: "Codex",
            detect: detect_codex,
            import: import_codex,
        },
        Source {
            id: "claude",
            name: "Claude Code",
            detect: detect_claude,
            import: import_claude,
        },
        Source {
            id: "gemini",
            name: "Gemini CLI",
            detect: detect_gemini,
            import: import_gemini,
        },
    ]
}

/// `killi import [tool] [--force] [--list]`: copies another coding agent's config. Without
/// a tool it imports the one agent found on the machine, or asks which when several are
/// (interactive only); `--list` shows what killi reads from and what it found.
pub fn import_command(args: &[String]) -> Result<()> {
    let force = args.iter().any(|arg| arg == "--force");
    if args.iter().any(|arg| arg == "--list") {
        for source in sources() {
            match (source.detect)() {
                Some(path) => println!("{:8} {} ({})", source.id, source.name, path.display()),
                None => println!("{:8} {} (-)", source.id, source.name),
            }
        }
        return Ok(());
    }
    let all = sources();
    if let Some(want) = args.iter().find(|arg| !arg.starts_with('-')) {
        let Some(source) = all.iter().find(|source| source.id == want) else {
            bail!("unknown tool `{want}`; killi imports from: {}", ids(&all))
        };
        return (source.import)(force);
    }
    let found: Vec<&Source> = all.iter().filter(|s| (s.detect)().is_some()).collect();
    match found.len() {
        0 => bail!(
            "no other coding agent's config found; killi imports from: {}",
            ids(&all)
        ),
        1 => (found[0].import)(force),
        _ => {
            if !std::io::stdin().is_terminal() {
                bail!(
                    "several configs found; name one: killi import <tool> ({})",
                    ids(&all)
                );
            }
            println!("Found several configs to import from:");
            for (index, source) in found.iter().enumerate() {
                println!(
                    "  {}. {} ({})",
                    index + 1,
                    source.name,
                    (source.detect)().unwrap_or_default().display()
                );
            }
            match read_line("Import from which? ")?.parse::<usize>() {
                Ok(choice) if (1..=found.len()).contains(&choice) => {
                    (found[choice - 1].import)(force)
                }
                _ => bail!("no import chosen"),
            }
        }
    }
}

/// The source ids, comma-separated, for the messages above.
fn ids(all: &[Source]) -> String {
    all.iter()
        .map(|source| source.id)
        .collect::<Vec<_>>()
        .join(", ")
}

fn present(path: PathBuf) -> Option<PathBuf> {
    path.exists().then_some(path)
}

fn detect_kimi() -> Option<PathBuf> {
    present(home().join(".kimi-code/config.toml"))
}

fn detect_pi() -> Option<PathBuf> {
    present(home().join(".pi/agent/models.json"))
}

fn detect_omp() -> Option<PathBuf> {
    ["models.yml", "models.yaml", "models.json"]
        .into_iter()
        .map(|name| home().join(".omp/agent").join(name))
        .find(|path| path.exists())
}

fn detect_opencode() -> Option<PathBuf> {
    present(home().join(".config/opencode/opencode.json"))
}

fn detect_codex() -> Option<PathBuf> {
    present(home().join(".codex/config.toml"))
}

fn detect_claude() -> Option<PathBuf> {
    present(home().join(".claude/settings.json"))
}

fn detect_gemini() -> Option<PathBuf> {
    present(home().join(".muse/settings.json"))
}

/// Writes an imported config with the `[prompt]` table, and copies the source agent's MCP
/// servers when killi has none. Every importer ends here.
fn write_import(
    out: toml::Table,
    mcp_src: Option<&Path>,
    dst_dir: &Path,
    force: bool,
) -> Result<()> {
    let dst = dst_dir.join("config.toml");
    if dst.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite it",
            dst.display()
        );
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dst_dir)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&dst)?;
    file.write_all(toml::to_string(&out)?.as_bytes())?;
    file.write_all(prompt_section().as_bytes())?;
    if let Some(src) = mcp_src.filter(|src| src.exists()) {
        let mcp_dst = dst_dir.join("mcp.json");
        if !mcp_dst.exists() || force {
            fs::copy(src, &mcp_dst).with_context(|| {
                format!("could not copy {} to {}", src.display(), mcp_dst.display())
            })?;
            println!("Copied MCP servers into {}", mcp_dst.display());
        }
    }
    Ok(())
}

/// Writes MCP servers in killi's `{"mcpServers": ...}` shape, which may carry secrets, so
/// the file is 0600 like the config. Nothing happens when there are no servers, or when
/// killi already has some and the import isn't forced.
fn write_mcp_json(
    servers: &serde_json::Map<String, serde_json::Value>,
    dst_dir: &Path,
    force: bool,
) -> Result<()> {
    if servers.is_empty() {
        return Ok(());
    }
    let dst = dst_dir.join("mcp.json");
    if dst.exists() && !force {
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dst_dir)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&dst)
        .with_context(|| format!("could not write {}", dst.display()))?;
    let text = serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": servers }))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("could not write {}", dst.display()))?;
    println!("Copied MCP servers into {}", dst.display());
    Ok(())
}

/// Reads a JSON file, or `None` when there isn't one.
fn read_json(path: &Path) -> Result<Option<serde_json::Value>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    Ok(Some(serde_json::from_str(&text).with_context(|| {
        format!("invalid JSON in {}", path.display())
    })?))
}

/// Reads a TOML file, or `None` when there isn't one.
fn read_toml(path: &Path) -> Result<Option<toml::Table>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", path.display()));
        }
    };
    Ok(Some(toml::from_str(&text).with_context(|| {
        format!("invalid TOML in {}", path.display())
    })?))
}

/// A set, non-blank process variable.
fn live_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// A JSON object's string field, when it is set and not blank.
fn json_str(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(String::from)
        .filter(|text| !text.is_empty())
}

/// A JSON object's number field.
fn json_u64(value: &serde_json::Value, field: &str) -> Option<u64> {
    value.get(field).and_then(serde_json::Value::as_u64)
}
