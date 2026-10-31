//! Meta's Muse Code: a subscription billed to the Muse Code client. Its API serves the
//! subscription only on the Responses API, and only without another client's tool names:
//! a request whose tools include `bash`, `shell`, `web_search` or `search` is answered with
//! `client_attestation_denied`, so killi sends and reads the mapped names instead.

use super::*;
use anyhow::Result;

/// Where Muse Code's models answer.
pub const BASE_URL: &str = "https://api.meta.ai/v1";

/// What `/provider muse-code` and `killi login muse-code` write, said before the key is asked.
pub const ABOUT: &str = "Writes a `meta` provider on the Responses API with the key, or repairs the one the config has, and maps the tool names Meta's client check refuses: bash → run_command, web_search → search_web. The key is the muse CLI's, `LLM|…`; a wrong one shows as HTTP 401 on the first request.";

/// The configured provider that is Muse Code: the one on Meta's API.
pub fn ours(provider: &Provider) -> bool {
    provider
        .base_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://api.meta.ai"))
}

/// `/provider muse-code`: writes the provider and its models with `api_key`. A Muse Code
/// provider the config has already is repaired: it gets the preset's API, URL and tool names.
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
        comment: "Meta's Muse Code, added with /provider muse-code or `killi login muse-code`.",
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
    fn a_config_without_meta_gets_the_provider_and_its_mapping() {
        let mine = "default_model = \"openai/gpt\"\n\n[providers.openai]\ntype = \"openai\"\napi_key = \"sk\"\n\n[models.\"openai/gpt\"]\nprovider = \"openai\"\nmodel = \"gpt\"\n";
        let (out, name, added, repaired) = with_additions(mine, &additions("LLM|1|k")).unwrap();
        assert_eq!((name.as_str(), added.len()), ("meta", 2));
        assert!(repaired.is_empty(), "nothing was there to repair");
        assert!(out.starts_with(mine), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let model = config
            .resolve(Some("meta/muse-wonder-1.0-contributor"))
            .unwrap();
        assert_eq!(model.protocol, Protocol::Responses);
        assert_eq!(model.base_url, BASE_URL);
        assert_eq!(model.api_key, "LLM|1|k");
        assert_eq!(model.tool_names["bash"], "run_command");
        assert_eq!(model.tool_names["web_search"], "search_web");
        assert_eq!(config.default_model.as_deref(), Some("openai/gpt"));
    }

    #[test]
    fn a_meta_provider_the_config_had_is_repaired_and_keeps_the_rest() {
        let broken = "default_model = \"meta/muse-wonder-1.0-contributor\"\n\n\
            # mine\n[providers.meta]\ntype = \"openai\"\nbase_url = \"https://api.meta.ai/v1\"\napi_key_env = \"MUSE_KEY\"\ncustom_headers = { x-team = \"a\" }\n\n\
            [providers.meta.tool_names]\nshell = \"run_shell\"\n\n\
            # my model\n[models.\"meta/muse-wonder-1.0-contributor\"]\nprovider = \"meta\"\nmodel = \"muse-wonder-1.0-contributor\"\ndefault_effort = \"high\"\n";
        let (out, name, added, repaired) = with_additions(broken, &additions("LLM|2|new")).unwrap();
        assert_eq!(name, "meta");
        assert_eq!(added, ["meta/muse-wonder-full-1.0-contributor"]);
        assert_eq!(repaired, ["tool_names", "type"], "{out}");
        assert!(out.contains("# mine\n[providers.meta]\n"), "{out}");
        assert!(out.contains("# my model\n"), "{out}");
        assert!(
            out.contains("tool_names = { bash = \"run_command\", shell = \"run_shell\", web_search = \"search_web\" }\n"),
            "one inline line, in place of the sub-table: {out}"
        );
        assert!(!out.contains("[providers.meta.tool_names]"), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        let meta = &config.providers["meta"];
        assert_eq!(meta.kind, "openai_responses");
        assert_eq!(
            (meta.api_key.as_deref(), meta.api_key_env.as_deref()),
            (Some("LLM|2|new"), None)
        );
        assert_eq!(
            meta.tool_names,
            BTreeMap::from([
                ("bash".to_string(), "run_command".to_string()),
                ("shell".to_string(), "run_shell".to_string()),
                ("web_search".to_string(), "search_web".to_string()),
            ]),
            "the user's own mapping stays: {out}"
        );
        assert_eq!(meta.custom_headers["x-team"], "a");
        let model = config.resolve(None).unwrap();
        assert_eq!(model.protocol, Protocol::Responses);
        assert_eq!(model.tool_names["web_search"], "search_web");

        let (again, _, none, nothing) = with_additions(&out, &additions("LLM|2|new")).unwrap();
        assert!(
            again == out && none.is_empty() && nothing.is_empty(),
            "a repaired config needs nothing more: {again}"
        );
    }

    #[test]
    fn a_meta_provider_on_another_api_is_left_alone() {
        let llama = "[providers.meta]\ntype = \"openai\"\nbase_url = \"https://api.llama.com/v1\"\napi_key = \"llama\"\n";
        let (out, name, _, repaired) = with_additions(llama, &additions("LLM|1|k")).unwrap();
        assert_eq!((name.as_str(), repaired.len()), ("meta-2", 0));
        assert!(out.contains(llama), "{out}");
        let config: Config = toml::from_str(&out).unwrap();
        assert_eq!(config.providers["meta"].api_key.as_deref(), Some("llama"));
        assert_eq!(config.providers["meta"].kind, "openai");
        assert_eq!(config.providers["meta-2"].tool_names["bash"], "run_command");
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
