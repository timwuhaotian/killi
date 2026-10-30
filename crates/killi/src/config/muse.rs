//! Meta's Muse Code: a subscription billed to the Muse Code client. Its API serves the
//! subscription only on the Responses API, and only without another client's tool names:
//! a request whose tools include `bash`, `shell`, `web_search` or `search` is answered with
//! `client_attestation_denied`, so killi sends and reads the mapped names instead.

use super::*;
use anyhow::Result;

/// Where Muse Code's models answer.
pub const BASE_URL: &str = "https://api.meta.ai/v1";

/// The configured provider that is Muse Code: the one on Meta's API.
pub fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://api.meta.ai"))
}

/// `/provider muse-code`: writes the provider and its models with `api_key`.
pub fn add(api_key: &str) -> Result<Added> {
    add_models(&additions(api_key))
}

/// The provider and models the preset writes: the Responses API on Meta's URL, the tool
/// names its client check demands, and the Muse Code models.
fn additions(api_key: &str) -> Additions {
    let mut tool_names = toml::Table::new();
    tool_names.insert("bash".into(), "run_command".into());
    tool_names.insert("web_search".into(), "search_web".into());
    let mut provider = toml::Table::new();
    provider.insert("type".into(), "openai_responses".into());
    provider.insert("base_url".into(), BASE_URL.into());
    provider.insert("tool_names".into(), tool_names.into());
    provider.insert("api_key".into(), api_key.into());
    Additions {
        name: "meta",
        ours,
        provider,
        models: vec![
            wonder("muse-wonder-1.0-contributor", "muse wonder"),
            wonder("muse-wonder-full-1.0-contributor", "muse wonder full"),
        ],
        comment: "Meta's Muse Code, added with /provider muse-code.",
    }
}

fn wonder(id: &'static str, display: &'static str) -> NewModel {
    NewModel {
        id: id.into(),
        context: Some(1_000_000),
        output: Some(128_000),
        images: true,
        tools: true,
        more_capabilities: vec!["video_in", "audio_in", "always_thinking"],
        display: Some(display),
        default_effort: Some("high".into()),
        efforts: vec!["high".into()],
        ..NewModel::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preset, rendered into an empty config the way `add_models` writes it.
    fn written() -> String {
        let (out, _, _, added) = models::with_models("", &additions("LLM|1|k")).unwrap();
        assert_eq!(
            added,
            [
                "meta/muse-wonder-1.0-contributor",
                "meta/muse-wonder-full-1.0-contributor"
            ]
        );
        out
    }

    #[test]
    fn the_provider_maps_the_tool_names_meta_refuses() {
        let out = written();
        assert!(out.contains("type = \"openai_responses\""), "{out}");
        assert!(
            out.contains("base_url = \"https://api.meta.ai/v1\""),
            "{out}"
        );
        assert!(
            out.contains(
                "[providers.meta.tool_names]\nbash = \"run_command\"\nweb_search = \"search_web\""
            ),
            "{out}"
        );
    }

    #[test]
    fn the_models_keep_muse_code_capabilities_and_display_names() {
        let out = written();
        assert!(out.contains("display_name = \"muse wonder\""), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = &config.models["meta/muse-wonder-1.0-contributor"];
        assert_eq!(
            model.capabilities,
            [
                "image_in",
                "tool_use",
                "video_in",
                "audio_in",
                "always_thinking"
            ]
        );
        assert_eq!(model.default_effort.as_deref(), Some("high"));
    }
}
