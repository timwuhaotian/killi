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
    /// Capabilities beyond those three flags, such as a subscription's `always_thinking`.
    pub more_capabilities: Vec<&'static str>,
    /// The short name the model list shows, when the id isn't what users say.
    pub display: Option<&'static str>,
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

/// What adding a sign-in's provider and models changed: the config's path, the provider's
/// name, the model aliases it added (empty when they were all in already), the fields of a
/// provider the config had that were set to the preset's (`tool_names`, say), and the alias
/// a config without `default_model` now starts on.
pub struct Added {
    pub path: PathBuf,
    pub provider: String,
    pub aliases: Vec<String>,
    pub repaired: Vec<String>,
    pub starts_on: Option<String>,
}

impl Added {
    /// The report `killi login` prints.
    pub fn report(&self) -> String {
        let mut out = match self.aliases.is_empty() {
            true => format!("Its models are already in {}.", self.path.display()),
            false => format!(
                "Added to {}: {}.",
                self.path.display(),
                self.aliases.join(", ")
            ),
        };
        if let Some(note) = self.repair_note() {
            out.push_str(&format!("\n{note}"));
        }
        match &self.starts_on {
            _ if self.aliases.is_empty() => {}
            Some(default) if self.aliases.contains(default) => out.push_str(&format!(
                "\nkilli starts on `{default}`; /model picks another."
            )),
            _ => out.push_str(&format!(
                "\nStart on one with `killi -m {}`, or pick it with /model.",
                self.aliases[0]
            )),
        }
        out
    }

    /// What was set in the provider the config already had, when anything was.
    pub fn repair_note(&self) -> Option<String> {
        (!self.repaired.is_empty()).then(|| {
            format!(
                "Repaired [providers.{}]: set {}.",
                self.provider,
                self.repaired.join(", ")
            )
        })
    }
}

/// Adds a sign-in's provider and the models the config doesn't have yet. A provider already
/// there gets the fields the sign-in writes, its key and `tool_names` among them, and keeps
/// the rest of its table. A new config, or one without `default_model`, starts on the first
/// model added.
pub fn add_models(additions: &Additions) -> Result<Added> {
    let dir = killi_home();
    let path = dir.join("config.toml");
    let text = match fs::read_to_string(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        read => Some(read.with_context(|| format!("could not read {}", path.display()))?),
    };
    let (out, name, added, repaired) =
        with_additions(text.as_deref().unwrap_or_default(), additions)?;
    let keyed = KEY_FIELDS
        .iter()
        .any(|field| additions.provider.contains_key(*field));
    match &text {
        Some(text) if *text == out => {}
        Some(_) => crate::fs_atomic::write(&path, out.as_bytes(), keyed.then_some(0o600))
            .with_context(|| format!("could not write {}", path.display()))?,
        None => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&dir)?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(out.as_bytes())?;
            file.write_all(prompt_section().as_bytes())?;
        }
    }
    let config: Config = toml::from_str(&out)?;
    let starts_on = match config.default_model.as_deref() {
        Some(default) if added.iter().any(|alias| alias == default) => Some(default.to_string()),
        _ => None,
    };
    Ok(Added {
        path,
        provider: name,
        aliases: added,
        repaired,
        starts_on,
    })
}

/// The config text `add_models` writes, the provider's name, the aliases added, and the
/// fields besides the key that a provider the config had was given.
pub(super) fn with_additions(
    text: &str,
    additions: &Additions,
) -> Result<(String, String, Vec<String>, Vec<String>)> {
    let (out, name, existed, added) = with_models(text, additions)?;
    if !existed {
        return Ok((out, name, added, Vec::new()));
    }
    let (out, mut repaired) = with_provider_fields(&out, &name, &additions.provider)?;
    repaired.retain(|field| !KEY_FIELDS.contains(&field.as_str()));
    Ok((out, name, added, repaired))
}

/// The config text with the additions at the end and the rest left as it is, the provider's
/// name, whether the config had it already, and the aliases added.
pub(super) fn with_models(
    text: &str,
    additions: &Additions,
) -> Result<(String, String, bool, Vec<String>)> {
    let config: Config = toml::from_str(text).context("invalid config")?;
    let existing = config
        .find_provider(additions.ours)
        .map(|(name, _)| name.to_string());
    let name = existing.clone().unwrap_or_else(|| {
        let taken = |name: &String| config.providers.contains_key(name);
        std::iter::once(additions.name.to_string())
            .chain((2..).map(|n| format!("{}-{n}", additions.name)))
            .find(|name| !taken(name))
            .unwrap_or_default()
    });
    let strings = |items: Vec<&str>| {
        toml::Value::Array(
            items
                .into_iter()
                .map(|item| toml::Value::String(item.to_string()))
                .collect(),
        )
    };
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
        let capabilities = [
            (model.images, "image_in"),
            (model.thinking, "thinking"),
            (model.tools, "tool_use"),
        ];
        let capabilities: Vec<&str> = capabilities
            .into_iter()
            .filter(|(has, _)| *has)
            .map(|(_, name)| name)
            .chain(model.more_capabilities.iter().copied())
            .collect();
        entry.insert("capabilities".into(), strings(capabilities));
        if let Some(display) = model.display {
            entry.insert("display_name".into(), display.into());
        }
        if let Some(effort) = &model.default_effort {
            entry.insert("default_effort".into(), effort.clone().into());
        }
        if !model.efforts.is_empty() {
            entry.insert(
                "support_efforts".into(),
                strings(model.efforts.iter().map(String::as_str).collect()),
            );
        }
        entries.insert(alias.clone(), entry.into());
        added.push(alias);
    }
    if added.is_empty() && existing.is_some() {
        return Ok((text.to_string(), name, true, added));
    }
    let mut out = String::new();
    if config.default_model.is_none() && !added.is_empty() {
        out.push_str(&format!(
            "default_model = {}\n",
            toml::Value::String(added[0].clone())
        ));
    }
    out.push_str(text);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("\n# {}\n", additions.comment));
    if existing.is_none() {
        let providers = toml::Table::from_iter([(
            name.clone(),
            toml::Value::Table(additions.provider.clone()),
        )]);
        out.push_str(&toml::to_string(&toml::Table::from_iter([(
            "providers".to_string(),
            toml::Value::Table(providers),
        )]))?);
    }
    if !entries.is_empty() {
        out.push('\n');
        out.push_str(&toml::to_string(&toml::Table::from_iter([(
            "models".to_string(),
            toml::Value::Table(entries),
        )]))?);
    }
    toml::from_str::<Config>(&out).context("adding the models would break the config")?;
    Ok((out, name, existing.is_some(), added))
}

/// A token count as a TOML integer.
pub(super) fn toml_int(value: u64) -> toml::Value {
    toml::Value::Integer(value as i64)
}
