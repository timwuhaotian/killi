use super::*;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

use anyhow::{Context, Result};

/// A model a sign-in adds to the config, as its `[models]` table will say it.
#[derive(Debug, Default)]
pub struct NewModel {
    pub id: String,
    /// When it isn't the provider's `type`.
    pub protocol: Option<&'static str>,
    /// When it isn't the provider's `base_url`.
    pub base_url: Option<String>,
    pub context: Option<u64>,
    pub output: Option<u64>,
    pub images: bool,
    pub thinking: bool,
    pub tools: bool,
    pub default_effort: Option<String>,
    pub efforts: Vec<String>,
}

/// What a sign-in adds to the config: its provider and models.
pub struct Additions {
    /// The provider's name, when the config doesn't have it yet.
    pub name: &'static str,
    /// Finds the provider in a config that has it already, under any name.
    pub ours: fn(&Provider) -> bool,
    pub provider: toml::Table,
    pub models: Vec<NewModel>,
    /// The comment above the added tables.
    pub comment: &'static str,
}

/// Adds a sign-in's provider and the models the config doesn't have yet, and says what it
/// added. A provider already there keeps its table, except for a new `api_key`. A new config,
/// or one without `default_model`, starts on the first model added.
pub fn add_models(additions: &Additions) -> Result<()> {
    let dir = killi_home();
    let path = dir.join("config.toml");
    let text = match fs::read_to_string(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        read => Some(read.with_context(|| format!("could not read {}", path.display()))?),
    };
    let (out, name, existed, added) = with_models(text.as_deref().unwrap_or_default(), additions)?;
    match &text {
        Some(text) if *text == out => {}
        Some(_) => crate::fs_atomic::write(&path, out.as_bytes(), None).with_context(|| format!("could not write {}", path.display()))?,
        None => {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
            let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
            file.write_all(out.as_bytes())?;
            file.write_all(prompt_section().as_bytes())?;
        }
    }
    if existed && let Some(toml::Value::String(key)) = additions.provider.get("api_key") {
        set_provider_key(&path, &name, key)?;
    }
    if added.is_empty() {
        println!("Its models are already in {}.", path.display());
        return Ok(());
    }
    println!("Added to {}: {}.", path.display(), added.join(", "));
    let config: Config = toml::from_str(&out)?;
    match config.default_model.as_deref() {
        Some(default) if added.iter().any(|alias| alias == default) => println!("killi starts on `{default}`; /model picks another."),
        _ => println!("Start on one with `killi -m {}`, or pick it with /model.", added[0]),
    }
    Ok(())
}

/// The config text with the additions at the end and the rest left as it is, the provider's
/// name, whether the config had it already, and the aliases added.
pub(super) fn with_models(text: &str, additions: &Additions) -> Result<(String, String, bool, Vec<String>)> {
    let config: Config = toml::from_str(text).context("invalid config")?;
    let existing = config.find_provider(additions.ours).map(|(name, _)| name.to_string());
    let name = existing.clone().unwrap_or_else(|| {
        let taken = |name: &String| config.providers.contains_key(name);
        std::iter::once(additions.name.to_string()).chain((2..).map(|n| format!("{}-{n}", additions.name))).find(|name| !taken(name)).unwrap_or_default()
    });
    let strings = |items: Vec<&str>| toml::Value::Array(items.into_iter().map(|item| toml::Value::String(item.to_string())).collect());
    let mut entries = toml::Table::new();
    let mut added = Vec::new();
    for model in &additions.models {
        let alias = format!("{name}/{}", model.id);
        if config.models.contains_key(&alias) || entries.contains_key(&alias) {
            continue;
        }
        let mut entry = toml::Table::new();
        entry.insert("provider".into(), name.clone().into());
        entry.insert("model".into(), model.id.clone().into());
        if let Some(protocol) = model.protocol {
            entry.insert("protocol".into(), protocol.into());
        }
        if let Some(url) = &model.base_url {
            entry.insert("base_url".into(), url.clone().into());
        }
        if let Some(context) = model.context {
            entry.insert("max_context_size".into(), toml_int(context));
        }
        if let Some(output) = model.output {
            entry.insert("max_output_size".into(), toml_int(output));
        }
        let capabilities = [(model.images, "image_in"), (model.thinking, "thinking"), (model.tools, "tool_use")];
        entry.insert("capabilities".into(), strings(capabilities.into_iter().filter(|(has, _)| *has).map(|(_, name)| name).collect()));
        if let Some(effort) = &model.default_effort {
            entry.insert("default_effort".into(), effort.clone().into());
        }
        if !model.efforts.is_empty() {
            entry.insert("support_efforts".into(), strings(model.efforts.iter().map(String::as_str).collect()));
        }
        entries.insert(alias.clone(), entry.into());
        added.push(alias);
    }
    if added.is_empty() && existing.is_some() {
        return Ok((text.to_string(), name, true, added));
    }
    let mut out = String::new();
    if config.default_model.is_none() && !added.is_empty() {
        out.push_str(&format!("default_model = {}\n", toml::Value::String(added[0].clone())));
    }
    out.push_str(text);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\n# {}\n", additions.comment));
    if existing.is_none() {
        let providers = toml::Table::from_iter([(name.clone(), toml::Value::Table(additions.provider.clone()))]);
        out.push_str(&toml::to_string(&toml::Table::from_iter([("providers".to_string(), toml::Value::Table(providers))]))?);
    }
    if !entries.is_empty() {
        out.push('\n');
        out.push_str(&toml::to_string(&toml::Table::from_iter([("models".to_string(), toml::Value::Table(entries))]))?);
    }
    toml::from_str::<Config>(&out).context("adding the models would break the config")?;
    Ok((out, name, existing.is_some(), added))
}

/// A token count as a TOML integer.
pub(super) fn toml_int(value: u64) -> toml::Value {
    toml::Value::Integer(value as i64)
}
