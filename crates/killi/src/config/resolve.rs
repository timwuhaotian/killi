use super::*;
use std::fs;

use anyhow::{Context, Result, bail};

impl Config {
    pub fn load() -> Result<Self> {
        let path = killi_home().join("config.toml");
        let text = match fs::read_to_string(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => bail!(
                "no config yet at {}; run `killi init` to write one to edit, `killi import-kimi` to copy Kimi Code's, or `killi login` to start on a subscription",
                path.display()
            ),
            read => read.with_context(|| format!("could not read {}", path.display()))?,
        };
        toml::from_str(&text).with_context(|| format!("invalid config in {}", path.display()))
    }

    pub fn resolve(&self, requested: Option<&str>) -> Result<ResolvedModel> {
        let Some(alias) = requested.or(self.default_model.as_deref()) else {
            bail!(
                "no model chosen: pass -m <provider/model> or set default_model in ~/.killi/config.toml"
            );
        };
        let Some(model) = self.models.get(alias) else {
            bail!(
                "unknown model `{alias}`; models are listed under [models.\"...\"] in ~/.killi/config.toml"
            );
        };
        let Some(provider) = self.providers.get(&model.provider) else {
            bail!(
                "model `{alias}` names provider `{}`, which isn't configured",
                model.provider
            );
        };
        let chatgpt = provider.kind == "chatgpt";
        let protocol = match model.protocol.as_deref().unwrap_or(&provider.kind) {
            "openai" => Protocol::OpenAi,
            "anthropic" => Protocol::Anthropic,
            "openai_responses" | "chatgpt" => Protocol::Responses,
            other => bail!(
                "model `{alias}` uses the `{other}` API, which killi doesn't support; use openai, anthropic, openai_responses or chatgpt"
            ),
        };
        if chatgpt && protocol != Protocol::Responses {
            bail!(
                "model `{alias}` runs on a ChatGPT plan, which takes only the Responses API; remove its `protocol`"
            );
        }
        let base_url = match (
            model.base_url.as_ref().or(provider.base_url.as_ref()),
            protocol,
        ) {
            (Some(url), _) => url.clone(),
            (None, Protocol::Anthropic) => "https://api.anthropic.com".to_string(),
            (None, _) => "https://api.openai.com/v1".to_string(),
        };
        let api_key = match chatgpt {
            true => String::new(),
            false => provider
                .key()
                .ok_or_else(|| anyhow::anyhow!("{}", no_key(alias, &model.provider, provider)))?,
        };
        Ok(ResolvedModel {
            alias: alias.to_string(),
            model: model.model.clone(),
            protocol,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            headers: provider.custom_headers.clone(),
            tool_names: provider.tool_names.clone(),
            session_header: provider.session_header.clone(),
            max_output: model.max_output_size,
            context_window: model.max_context_size,
            images: model.capabilities.iter().any(|c| c == "image_in"),
            thinking: self.thinking_for(model),
            cache_control: model.cache_control,
            chatgpt,
        })
    }

    /// The model killi would start on when its provider has no key: the alias and the
    /// provider's name. `None` when that model has a key, or when something else is wrong
    /// with it, which `resolve` reports.
    pub fn missing_key(&self, requested: Option<&str>) -> Option<MissingKey> {
        let alias = requested.or(self.default_model.as_deref())?;
        let model = self.models.get(alias)?;
        let provider = self.providers.get(&model.provider)?;
        (provider.kind != "chatgpt" && provider.key().is_none()).then(|| MissingKey {
            alias: alias.to_string(),
            provider: model.provider.clone(),
        })
    }

    /// The configured provider `ours` picks out: its name, and whether it has a key.
    pub fn find_provider(&self, ours: fn(&Provider) -> bool) -> Option<(&str, bool)> {
        self.providers
            .iter()
            .find(|(_, provider)| ours(provider))
            .map(|(name, provider)| (name.as_str(), provider.key().is_some()))
    }

    /// The model killi would start on, when it runs on a ChatGPT plan.
    pub fn chatgpt_model<'a>(&'a self, requested: Option<&'a str>) -> Option<&'a str> {
        let alias = requested.or(self.default_model.as_deref())?;
        let model = self.models.get(alias)?;
        (self.providers.get(&model.provider)?.kind == "chatgpt").then_some(alias)
    }

    /// `[thinking] enabled = false` turns reasoning off, then the model's `default_effort`
    /// (where `/effort` keeps the last pick), then the middle of its supported efforts.
    fn thinking_for(&self, model: &Model) -> Thinking {
        if self.thinking.enabled == Some(false) {
            return Thinking::Off;
        }
        let effort = model.default_effort.clone().or_else(|| {
            let efforts = &model.support_efforts;
            (!efforts.is_empty()).then(|| efforts[efforts.len() / 2].clone())
        });
        effort.as_deref().map_or(Thinking::On, Thinking::parse)
    }

    /// The system prompt's guidance: `[prompt] system`, or the built-in text.
    pub fn guidance(&self) -> &str {
        self.prompt
            .system
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or(crate::prompt::DEFAULT)
    }

    /// The efforts `/effort` offers for a model, as Kimi Code lists them: off, then the
    /// model's `support_efforts`, or on/off and the common levels when it declares none.
    pub fn efforts(&self, alias: &str) -> Vec<String> {
        let declared = self
            .models
            .get(alias)
            .map(|m| m.support_efforts.clone())
            .unwrap_or_default();
        let levels: Vec<&str> = if declared.is_empty() {
            vec!["on", "low", "medium", "high"]
        } else {
            Vec::new()
        };
        std::iter::once("off")
            .chain(levels)
            .map(String::from)
            .chain(declared)
            .collect()
    }
}

impl Thinking {
    pub fn parse(effort: &str) -> Self {
        match effort {
            "on" => Thinking::On,
            "off" => Thinking::Off,
            effort => Thinking::Effort(effort.to_string()),
        }
    }

    /// The name `parse` reads back.
    pub fn label(&self) -> &str {
        match self {
            Thinking::Off => "off",
            Thinking::On => "on",
            Thinking::Effort(effort) => effort,
        }
    }
}
