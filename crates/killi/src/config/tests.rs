mod import;

use super::{Config, import_omp_from, read_yaml};

#[test]
fn template_parses_and_resolves() {
    let config: super::Config = toml::from_str(super::TEMPLATE).unwrap();
    assert_eq!(config.models.len(), 2);
    assert_eq!(config.agent.swarm, Some(true));
    // SAFETY: tests that read this variable don't run concurrently with one that changes it.
    unsafe { std::env::set_var("OPENAI_API_KEY", "test-key") };
    let model = config.resolve(None).unwrap();
    assert_eq!(model.protocol, super::Protocol::Responses);
    assert_eq!(model.thinking, super::Thinking::On, "no global effort any more: the model's own, else the provider's default");
}

#[test]
fn thinking_follows_the_model_and_ignores_a_former_global_effort() {
    let base = "[providers.test]\ntype = \"openai\"\napi_key = \"k\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n";
    let off: super::Config = toml::from_str(&format!("[thinking]\nenabled = false\n{base}")).unwrap();
    assert_eq!(off.resolve(Some("test/model")).unwrap().thinking, super::Thinking::Off);
    // An `effort` line from before the change is unknown and changes nothing.
    let legacy: super::Config = toml::from_str(&format!("[thinking]\neffort = \"high\"\n{base}")).unwrap();
    assert_eq!(legacy.resolve(Some("test/model")).unwrap().thinking, super::Thinking::On);

    let own: super::Config = toml::from_str(&format!("{base}default_effort = \"low\"\nsupport_efforts = [\"low\", \"medium\", \"high\"]\n")).unwrap();
    assert_eq!(own.resolve(Some("test/model")).unwrap().thinking, super::Thinking::Effort("low".into()), "the model's default wins");

    let middle: super::Config = toml::from_str(&format!("{base}support_efforts = [\"low\", \"medium\", \"high\"]\n")).unwrap();
    assert_eq!(middle.resolve(Some("test/model")).unwrap().thinking, super::Thinking::Effort("medium".into()), "without one, the middle of the supported efforts");
}

#[test]
fn a_chatgpt_model_needs_no_key_and_speaks_responses() {
    let source = "[providers.plan]\ntype = \"chatgpt\"\n\n[models.\"plan/gpt-a\"]\nprovider = \"plan\"\nmodel = \"gpt-a\"\nmax_output_size = 1000\n";
    let config: super::Config = toml::from_str(source).unwrap();
    let model = config.resolve(Some("plan/gpt-a")).unwrap();
    assert_eq!(model.protocol, super::Protocol::Responses);
    assert!(model.chatgpt && model.api_key.is_empty());
    assert_eq!(model.base_url, "https://api.openai.com/v1");
    assert!(config.missing_key(Some("plan/gpt-a")).is_none(), "the first-run guide doesn't ask for a key");
    assert_eq!(config.chatgpt_model(Some("plan/gpt-a")), Some("plan/gpt-a"));

    let chat: super::Config = toml::from_str(&format!("{source}protocol = \"openai\"\n")).unwrap();
    assert!(chat.resolve(Some("plan/gpt-a")).unwrap_err().to_string().contains("only the Responses API"));
    let template: super::Config = toml::from_str(super::TEMPLATE).unwrap();
    assert_eq!(template.chatgpt_model(None), None);
}

fn plan_model(id: &str) -> super::NewModel {
    super::NewModel {
        id: id.into(),
        context: Some(272000),
        images: true,
        thinking: true,
        tools: true,
        efforts: vec!["low".into(), "high".into()],
        default_effort: Some("low".into()),
        ..super::NewModel::default()
    }
}

fn chatgpt(models: Vec<super::NewModel>) -> super::Additions {
    super::Additions {
        name: "chatgpt",
        ours: |provider| provider.kind == "chatgpt",
        provider: toml::Table::from_iter([("type".to_string(), toml::Value::from("chatgpt"))]),
        models,
        comment: "Your ChatGPT plan.",
    }
}

#[test]
fn signing_in_adds_the_provider_and_its_models_once() {
    let (text, name, existed, added) = super::with_models("", &chatgpt(vec![plan_model("gpt-a"), plan_model("gpt-b")])).unwrap();
    assert_eq!((name.as_str(), existed), ("chatgpt", false));
    assert_eq!(added, ["chatgpt/gpt-a", "chatgpt/gpt-b"]);
    let config: super::Config = toml::from_str(&text).unwrap();
    assert_eq!(config.default_model.as_deref(), Some("chatgpt/gpt-a"));
    assert_eq!(config.providers["chatgpt"].kind, "chatgpt");
    let model = &config.models["chatgpt/gpt-a"];
    assert_eq!((model.max_context_size, model.default_effort.as_deref()), (Some(272000), Some("low")));
    assert_eq!(model.support_efforts, ["low", "high"]);
    assert_eq!(model.capabilities, ["image_in", "thinking", "tool_use"]);
    assert!(config.resolve(None).unwrap().chatgpt);

    let (again, _, existed, none) = super::with_models(&text, &chatgpt(vec![plan_model("gpt-a")])).unwrap();
    assert!(existed && none.is_empty() && again == text, "nothing is added twice");
    let (more, _, _, added) = super::with_models(&text, &chatgpt(vec![plan_model("gpt-a"), plan_model("gpt-c")])).unwrap();
    assert_eq!(added, ["chatgpt/gpt-c"]);
    assert_eq!(more.matches("[providers.chatgpt]").count(), 1, "the provider is reused: {more}");
}

#[test]
fn added_models_go_after_the_users_config_and_keep_it() {
    let mine = "# mine\ndefault_model = \"openai/gpt-5.5\"\n\n[providers.chatgpt]\ntype = \"openai\"\napi_key = \"k\"";
    let (text, name, _, added) = super::with_models(mine, &chatgpt(vec![plan_model("gpt-a")])).unwrap();
    assert!(text.starts_with(mine), "{text}");
    assert_eq!((name.as_str(), added.as_slice()), ("chatgpt-2", ["chatgpt-2/gpt-a".to_string()].as_slice()), "a `chatgpt` provider of another type keeps its name");
    let config: super::Config = toml::from_str(&text).unwrap();
    assert_eq!(config.default_model.as_deref(), Some("openai/gpt-5.5"));
    assert_eq!(config.providers["chatgpt-2"].kind, "chatgpt");
}

#[test]
fn a_model_can_answer_on_its_own_url_and_protocol() {
    let source = "[providers.go]\ntype = \"openai\"\nbase_url = \"https://gateway.example/openai/v1\"\napi_key = \"k\"\nsession_header = \"x-opencode-session\"\n\n\
                  [models.\"go/kimi\"]\nprovider = \"go\"\nmodel = \"kimi\"\n\n\
                      [models.\"go/haiku\"]\nprovider = \"go\"\nmodel = \"haiku\"\nprotocol = \"anthropic\"\nbase_url = \"https://gateway.example/anthropic\"\n";
        let config: super::Config = toml::from_str(source).unwrap();
        let kimi = config.resolve(Some("go/kimi")).unwrap();
    assert_eq!((kimi.protocol, kimi.base_url.as_str()), (super::Protocol::OpenAi, "https://gateway.example/openai/v1"));
    assert_eq!(kimi.session_header.as_deref(), Some("x-opencode-session"));
    let haiku = config.resolve(Some("go/haiku")).unwrap();
    assert_eq!((haiku.protocol, haiku.base_url.as_str(), haiku.api_key.as_str()), (super::Protocol::Anthropic, "https://gateway.example/anthropic", "k"));
}

#[test]
fn signing_out_takes_only_that_providers_key_lines() {
    let root = std::env::temp_dir().join(format!("killi-unkey-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    std::fs::write(&path, "# mine\n[providers.go]\ntype = \"openai\"\napi_key = \"secret\"\nbase_url = \"u\"\n\n[providers.other]\ntype = \"openai\"\napi_key = \"keep\"\n").unwrap();
    assert!(super::remove_provider_key(&path, "go").unwrap());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("secret") && text.contains("keep") && text.contains("base_url = \"u\"") && text.contains("# mine"), "{text}");
    assert!(!super::remove_provider_key(&path, "go").unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn debug_redacts_the_key() {
    let source = "[providers.test]\ntype = \"openai\"\nbase_url = \"https://api.openai.com/v1\"\napi_key = \"sk-live-key-material\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n";
    let config: super::Config = toml::from_str(source).unwrap();
    let shown = format!("{:?}", config.resolve(Some("test/model")).unwrap());
    assert!(!shown.contains("sk-live-key-material"), "{shown}");
    assert!(shown.contains("test/model"), "{shown}");
    assert!(shown.contains(&format!("{} chars", "sk-live-key-material".len())), "{shown}");
}

#[test]
fn swarm_mode_starts_on_unless_the_config_says_off() {
    assert!(super::AgentConfig::default().swarm_on());
    let off: super::Config = toml::from_str("[agent]\nswarm = false\n").unwrap();
    assert!(!off.agent.swarm_on());
}

#[test]
fn a_missing_key_variable_names_the_variable_and_the_fix() {
    let config: super::Config = toml::from_str(
        "default_model = \"test/model\"\n\n[providers.test]\ntype = \"openai\"\napi_key_env = \"KILLI_TEST_ABSENT_KEY\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n",
    )
    .unwrap();
    let error = config.resolve(None).unwrap_err().to_string();
    assert!(error.contains("model `test/model`"), "{error}");
    assert!(error.contains("$KILLI_TEST_ABSENT_KEY"), "{error}");
    assert!(error.contains("[providers.test]"), "{error}");
    assert!(error.contains("~/.killi/config.toml"), "{error}");
    assert!(!error.contains("environment variable not found"), "{error}");
}

#[test]
fn an_empty_key_variable_counts_as_missing() {
    // SAFETY: no other test reads or writes KILLI_TEST_EMPTY_KEY.
    unsafe { std::env::set_var("KILLI_TEST_EMPTY_KEY", "") };
    let config: super::Config = toml::from_str(
        "default_model = \"test/model\"\n\n[providers.test]\ntype = \"openai\"\napi_key_env = \"KILLI_TEST_EMPTY_KEY\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n",
    )
    .unwrap();
    let error = config.resolve(None).unwrap_err().to_string();
    assert!(error.contains("$KILLI_TEST_EMPTY_KEY"), "{error}");
}

#[test]
fn missing_key_names_the_model_and_provider_that_cant_start() {
    let source = "[providers.test]\ntype = \"openai\"\napi_key_env = \"KILLI_TEST_ABSENT_KEY\"\n\n[providers.inline]\ntype = \"openai\"\napi_key = \"sk-inline\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n\n[models.\"inline/model\"]\nprovider = \"inline\"\nmodel = \"model\"\n";
    let config: super::Config = toml::from_str(&format!("default_model = \"test/model\"\n{source}")).unwrap();
    let missing = config.missing_key(None).unwrap();
    assert_eq!(missing.alias, "test/model");
    assert_eq!(missing.provider, "test");
    assert!(config.missing_key(Some("inline/model")).is_none(), "an inline key is a key");
    assert!(config.missing_key(Some("unknown/model")).is_none(), "resolve reports those");

    let blank: super::Config = toml::from_str("default_model = \"test/model\"\n\n[providers.test]\ntype = \"openai\"\napi_key = \"\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n").unwrap();
    assert!(blank.missing_key(None).is_some(), "a blank key is no key");
}

#[test]
fn storing_a_key_replaces_the_environment_line_and_keeps_the_rest() {
    let root = std::env::temp_dir().join(format!("killi-key-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    std::fs::write(&path, "# my config\ndefault_model = \"test/model\"\n\n[providers.test]\ntype = \"openai\"\napi_key_env = \"KILLI_TEST_ABSENT_KEY\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n").unwrap();

    super::set_provider_key(&path, "test", "sk-pasted").unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# my config"), "{text}");
    assert!(!text.contains("api_key_env"), "{text}");
    let config: super::Config = toml::from_str(&text).unwrap();
    assert_eq!(config.providers["test"].api_key.as_deref(), Some("sk-pasted"));
    assert!(config.missing_key(None).is_none());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn setting_search_replaces_the_table_and_keeps_the_rest() {
    let root = std::env::temp_dir().join(format!("killi-search-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    std::fs::write(&path, "# my config\ndefault_model = \"test/model\"\n\n[search]\nprovider = \"tavily\"\napi_key_env = \"TAVILY_KEY\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n").unwrap();

    super::set_search(&path, "searxng", Some("http://localhost:8080"), None).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# my config\ndefault_model = \"test/model\"\n\n[search]\nprovider = \"searxng\"\nurl = \"http://localhost:8080\"\n"), "{text}");
    assert!(!text.contains("TAVILY_KEY"), "a key set for another provider is dropped: {text}");
    assert!(text.contains("[models.\"test/model\"]"), "{text}");
    let config: super::Config = toml::from_str(&text).unwrap();
    assert_eq!((config.search.provider.as_deref(), config.search.url.as_deref()), (Some("searxng"), Some("http://localhost:8080")));

    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn setting_search_adds_the_table_at_the_end() {
    let root = std::env::temp_dir().join(format!("killi-search-add-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    std::fs::write(&path, "default_model = \"test/model\"\n").unwrap();

    super::set_search(&path, "brave", None, Some("the-key")).unwrap();
    let config: super::Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!((config.search.provider.as_deref(), config.search.url.as_deref(), config.search.api_key.as_deref()), (Some("brave"), None, Some("the-key")));

    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn choosing_a_model_rewrites_the_default_model_line() {
    let root = std::env::temp_dir().join(format!("killi-default-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    std::fs::write(&path, "default_model = \"test/first\"\n\n[models.\"test/first\"]\nprovider = \"test\"\nmodel = \"first\"\n").unwrap();
    super::set_default_model(&path, "test/second").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "default_model = \"test/second\"\n\n[models.\"test/first\"]\nprovider = \"test\"\nmodel = \"first\"\n");

    std::fs::write(&path, "[models.\"test/first\"]\nprovider = \"test\"\nmodel = \"first\"\n").unwrap();
    super::set_default_model(&path, "test/first").unwrap();
    let config: super::Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(config.default_model.as_deref(), Some("test/first"));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn setting_a_backup_model_rewrites_the_line_and_none_removes_it() {
    let root = std::env::temp_dir().join(format!("killi-backup-model-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    std::fs::write(&path, "# my config\ndefault_model = \"test/first\"\n\n[models.\"test/first\"]\nprovider = \"test\"\nmodel = \"first\"\n").unwrap();

    super::set_backup_model(&path, Some("test/second")).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# my config"), "{text}");
    let config: super::Config = toml::from_str(&text).unwrap();
    assert_eq!(config.backup_model.as_deref(), Some("test/second"), "the line is added where the file had none");

    super::set_backup_model(&path, Some("test/first")).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("backup_model").count(), 1, "a second pick replaces the line, it doesn't add one: {text}");
    assert_eq!(toml::from_str::<super::Config>(&text).unwrap().backup_model.as_deref(), Some("test/first"));

    super::set_backup_model(&path, None).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("backup_model"), "none takes the line out: {text}");
    assert_eq!(toml::from_str::<super::Config>(&text).unwrap().backup_model, None);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn picking_an_effort_rewrites_the_models_default_effort_line() {
    let root = std::env::temp_dir().join(format!("killi-effort-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    std::fs::write(&path, "# my config\n\n[providers.test]\ntype = \"openai\"\napi_key = \"k\"\n\n[models.\"test/model\"]  # my model\nprovider = \"test\"\nmodel = \"model\"\ndefault_effort = \"high\"\n\n[models.\"test/other\"]\nprovider = \"test\"\nmodel = \"other\"\ndefault_effort = \"low\"\n").unwrap();

    super::set_model_effort(&path, "test/model", "medium").unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[models.\"test/model\"]  # my model\nprovider = \"test\"\nmodel = \"model\"\ndefault_effort = \"medium\""), "the line is replaced in place: {text}");
    assert!(text.contains("default_effort = \"low\""), "the other model keeps its own: {text}");
    let config: super::Config = toml::from_str(&text).unwrap();
    assert_eq!(config.models["test/model"].default_effort.as_deref(), Some("medium"));
    assert_eq!(config.resolve(Some("test/model")).unwrap().thinking, super::Thinking::Effort("medium".into()));

    std::fs::write(&path, "[providers.test]\ntype = \"openai\"\napi_key = \"k\"\n\n[models.'test/model']\nprovider = \"test\"\nmodel = \"model\"\n").unwrap();
    super::set_model_effort(&path, "test/model", "off").unwrap();
    let config: super::Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(config.resolve(Some("test/model")).unwrap().thinking, super::Thinking::Off, "a missing line is added under the header, quotes either way");

    assert!(super::set_model_effort(&path, "test/absent", "low").unwrap_err().to_string().contains("no [models.\"test/absent\"] table"));
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn importing_pi_carries_over_providers_models_and_the_default() {
    let root = std::env::temp_dir().join(format!("killi-import-pi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = root.join("pi");
    let dst = root.join("killi");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("models.json"), serde_json::json!({"providers": {
        "sph": {"baseUrl": "https://gw.example.com/v1", "api": "openai-completions", "apiKey": "sk-sph", "models": [
            {"id": "glm-5.3", "reasoning": true, "input": ["text", "image"], "contextWindow": 1000000, "maxTokens": 128000}
        ]},
        "minimax": {"apiKey": "sk-mm"},
        "nokey": {"baseUrl": "https://x/v1", "models": [{"id": "m"}]}
    }}).to_string()).unwrap();
    std::fs::write(src.join("models-store.json"), serde_json::json!({"minimax": {"models": [
        {"id": "MiniMax-M3", "api": "anthropic-messages", "baseUrl": "https://api.minimaxi.com/anthropic", "reasoning": true, "contextWindow": 196608, "maxTokens": 32768},
        {"id": "MiniMax-M3-Flash", "baseUrl": "https://api.minimaxi.com/anthropic"}
    ]}}).to_string()).unwrap();
    std::fs::write(src.join("settings.json"), serde_json::json!({"defaultProvider": "minimax", "defaultModel": "MiniMax-M3", "defaultThinkingLevel": "high"}).to_string()).unwrap();
    std::fs::write(src.join("mcp.json"), serde_json::json!({"mcpServers": {"x": {"command": "echo"}}}).to_string()).unwrap();

    super::import_pi_from(&src, &dst, false).unwrap();
    let text = std::fs::read_to_string(dst.join("config.toml")).unwrap();
    let config: super::Config = toml::from_str(&text).unwrap();
    assert_eq!(config.default_model.as_deref(), Some("minimax/MiniMax-M3"));
    assert_eq!(config.models["minimax/MiniMax-M3"].default_effort.as_deref(), Some("high"), "pi's thinking level is the default model's own");
    assert_eq!(config.providers.len(), 2, "a provider without a key is skipped");
    assert_eq!(config.providers["sph"].api_key.as_deref(), Some("sk-sph"));
    assert_eq!(config.providers["sph"].base_url.as_deref(), Some("https://gw.example.com/v1"));
    assert_eq!(config.providers["minimax"].kind, "anthropic", "the catalog model's API names the protocol");
    assert_eq!(config.providers["minimax"].base_url.as_deref(), Some("https://api.minimaxi.com/anthropic"));
    let glm = &config.models["sph/glm-5.3"];
    assert_eq!((glm.max_context_size, glm.max_output_size), (Some(1_000_000), Some(128_000)));
    assert_eq!(glm.capabilities, ["thinking", "image_in"]);
    assert_eq!(config.models["minimax/MiniMax-M3-Flash"].protocol, None, "a model that names no API takes its provider's");
    assert_eq!(std::fs::read_to_string(dst.join("mcp.json")).unwrap(), std::fs::read_to_string(src.join("mcp.json")).unwrap());
    assert!(config.missing_key(None).is_none(), "the imported config starts as it is");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn importing_opencode_carries_over_providers_models_and_mcp() {
    let root = std::env::temp_dir().join(format!("killi-import-opencode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = root.join("opencode");
    let dst = root.join("killi");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("opencode.json"), serde_json::json!({
        "model": "minimax/MiniMax-M3",
        "provider": {
            "sph": {"npm": "@ai-sdk/openai-compatible", "options": {"baseURL": "https://gw.example.com/v1", "apiKey": "sk-sph"}, "models": {
                "glm-5.3": {"name": "GLM 5.3", "reasoning": true, "attachment": true, "limit": {"context": 1000000, "output": 128000}}
            }},
            "volc": {"options": {"baseURL": "https://ark.example.com/v3"}, "models": {"doubao": {}}},
            "oauthonly": {"models": {"m": {}}}
        },
        "mcp": {
            "local": {"type": "local", "command": ["npx", "-y", "pkg"]},
            "remote": {"type": "remote", "url": "https://mcp.example.com"},
            "off": {"type": "local", "command": ["x"], "enabled": false}
        }
    }).to_string()).unwrap();
    std::fs::write(src.join("auth.json"), serde_json::json!({
        "volc": {"type": "api", "key": "sk-volc"},
        "oauthonly": {"type": "oauth", "refresh": "r"}
    }).to_string()).unwrap();

    super::import_opencode_from(&src.join("opencode.json"), &src.join("auth.json"), &dst, false).unwrap();
    let config: super::Config = toml::from_str(&std::fs::read_to_string(dst.join("config.toml")).unwrap()).unwrap();
    assert_eq!(config.default_model.as_deref(), Some("sph/glm-5.3"), "a default whose provider wasn't imported falls back");
    assert_eq!(config.providers.len(), 2);
    assert_eq!(config.providers["sph"].api_key.as_deref(), Some("sk-sph"));
    assert_eq!(config.providers["volc"].api_key.as_deref(), Some("sk-volc"), "the key comes from auth.json");
    assert_eq!(config.providers["volc"].kind, "openai");
    let glm = &config.models["sph/glm-5.3"];
    assert_eq!((glm.max_context_size, glm.max_output_size), (Some(1_000_000), Some(128_000)));
    assert_eq!(glm.capabilities, ["thinking", "image_in"]);
    assert_eq!(config.models["volc/doubao"].model, "doubao");

    let mcp: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dst.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["local"]["command"], "npx");
    assert_eq!(mcp["mcpServers"]["local"]["args"], serde_json::json!(["-y", "pkg"]));
    assert_eq!(mcp["mcpServers"]["remote"]["url"], "https://mcp.example.com");
    assert!(mcp["mcpServers"].get("off").is_none(), "a disabled server is left out");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn yes_means_enter_or_yes_and_end_of_input_means_no() {
    assert!(super::agree(&Some(String::new())), "Enter means yes");
    assert!(super::agree(&Some("y".into())));
    assert!(super::agree(&Some("YES".into())));
    assert!(!super::agree(&None), "end of input is no, so a closed guide writes nothing");
    assert!(!super::agree(&Some("n".into())));
    assert!(!super::agree(&Some("quit".into())));
}

#[test]
fn confirm_on_empty_stdin_is_no() {
    // stdin reads as end of input below; nothing else in this process reads it.
    // SAFETY: duplicates a /dev/null descriptor onto stdin and restores it right after.
    unsafe {
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY);
        assert!(null >= 0);
        let saved = libc::dup(libc::STDIN_FILENO);
        assert!(saved >= 0);
        assert_eq!(libc::dup2(null, libc::STDIN_FILENO), libc::STDIN_FILENO);
        let answer = super::confirm("Write a starting config to edit?");
        libc::dup2(saved, libc::STDIN_FILENO);
        libc::close(saved);
        libc::close(null);
        assert!(!answer.unwrap());
    }
}

#[test]
fn command_names_are_single_safe_words() {
    for name in ["dw", "ddd", "d", "my-cmd_2", "K9"] {
        assert!(super::valid_command_name(name), "{name}");
    }
    for name in ["", "-dw", "d w", "dw/killi", "dw;rm", "dw\n", &"d".repeat(33)] {
        assert!(!super::valid_command_name(name), "{name:?}");
    }
}

#[test]
fn command_link_points_at_the_binary_and_never_overwrites() {
    let root = std::env::temp_dir().join(format!("killi-command-guide-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let exe = root.join("killi");
    std::fs::write(&exe, b"binary").unwrap();
    let exe = std::fs::canonicalize(exe).unwrap();

    let link = super::create_command_link(&exe, "dwtest").unwrap();
    assert_eq!(std::fs::read_link(&link).unwrap(), std::path::Path::new("killi"));
    assert_eq!(super::create_command_link(&exe, "dwtest").unwrap(), link);

    std::fs::write(root.join("taken"), b"other").unwrap();
    assert!(super::create_command_link(&exe, "taken").unwrap_err().contains("already exists"));
    assert_eq!(std::fs::read(root.join("taken")).unwrap(), b"other");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn edits_keep_neighbours_and_reparse() {
    let root = std::env::temp_dir().join(format!("killi-edit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    let prompt_block = "system = '''\nYou are kil.\n[models.deep]\n[ providers.other ]\napi_key = \"not a table\"\n'''";
    let text = format!(
            "# my config\ndefault_model = \"test/model\"\nproviders.other.type = \"openai\"\nproviders.other.api_key = \"keep\"\n\n\
             [ providers.test ]  # spaces and a comment\ntype = \"openai\"\napi_key_env = \"KILLI_TEST_ABSENT_KEY\"\n\n\
             [models.\"test/model\"]  # my model\nprovider = \"test\"\nmodel = \"model\"\n\n\
             [prompt]\n{prompt_block}\n\n\
         [providers.second]\ntype = \"openai\"\napi_key = \"keep2\"\nbase_url = \"https://second.example\"\n"
    );
    std::fs::write(&path, &text).unwrap();

    super::set_provider_key(&path, "test", "sk-pasted").unwrap();
    let edited = std::fs::read_to_string(&path).unwrap();
    let config: super::Config = toml::from_str(&edited).unwrap();
    assert_eq!(config.providers["test"].api_key.as_deref(), Some("sk-pasted"));
    assert_eq!(config.providers["test"].api_key_env, None);
    assert_eq!(config.providers["other"].api_key.as_deref(), Some("keep"), "a dotted key is no table header: {edited}");
    assert_eq!(config.providers["second"].api_key.as_deref(), Some("keep2"), "the next provider keeps its key: {edited}");
    assert!(edited.contains("[ providers.test ]  # spaces and a comment\ntype = \"openai\"\napi_key = \"sk-pasted\""), "the key line takes the environment line's place: {edited}");
    assert!(!edited.contains("api_key_env"), "{edited}");
    assert!(edited.contains(&format!("[prompt]\n{prompt_block}")), "the prompt block is left as it is: {edited}");
    assert!(edited.contains("[models.\"test/model\"]  # my model"), "{edited}");

    assert!(super::remove_provider_key(&path, "test").unwrap());
    let edited = std::fs::read_to_string(&path).unwrap();
    let config: super::Config = toml::from_str(&edited).unwrap();
    assert_eq!((config.providers["test"].api_key.as_deref(), config.providers["test"].api_key_env.as_deref()), (None, None));
    assert_eq!(config.providers["second"].api_key.as_deref(), Some("keep2"), "{edited}");
    assert_eq!(config.providers["other"].api_key.as_deref(), Some("keep"), "{edited}");
    assert!(edited.contains("[ providers.test ]  # spaces and a comment"), "{edited}");
    assert!(edited.contains(&format!("[prompt]\n{prompt_block}")), "{edited}");
    assert!(!super::remove_provider_key(&path, "test").unwrap(), "there is no key left to take out");

    super::set_default_model(&path, "test/second").unwrap();
    let edited = std::fs::read_to_string(&path).unwrap();
    let config: super::Config = toml::from_str(&edited).unwrap();
    assert_eq!(config.default_model.as_deref(), Some("test/second"));
    assert_eq!(config.providers["second"].api_key.as_deref(), Some("keep2"), "{edited}");
    assert!(edited.contains(&format!("[prompt]\n{prompt_block}")), "{edited}");

    super::set_search(&path, "searxng", Some("http://localhost:8080"), None).unwrap();
    let edited = std::fs::read_to_string(&path).unwrap();
    let config: super::Config = toml::from_str(&edited).unwrap();
    assert_eq!((config.search.provider.as_deref(), config.search.url.as_deref()), (Some("searxng"), Some("http://localhost:8080")));
    assert!(edited.ends_with("\n[search]\nprovider = \"searxng\"\nurl = \"http://localhost:8080\"\n"), "{edited}");
    assert_eq!(config.providers["second"].api_key.as_deref(), Some("keep2"), "{edited}");
    assert!(edited.contains(&format!("[prompt]\n{prompt_block}")), "{edited}");

    let (added, _, _, aliases) = super::with_models(&edited, &chatgpt(vec![plan_model("gpt-a")])).unwrap();
    assert_eq!(aliases, ["chatgpt/gpt-a"]);
    let config: super::Config = toml::from_str(&added).unwrap();
    assert_eq!(config.providers["chatgpt"].kind, "chatgpt");
    assert_eq!(config.providers["second"].api_key.as_deref(), Some("keep2"), "{added}");
    assert!(added.contains(&format!("[prompt]\n{prompt_block}")), "{added}");
    assert!(added.starts_with("# my config\ndefault_model = \"test/second\""), "{added}");

    std::fs::write(&path, "default_model = \"test/model\"\nsearch.provider = \"brave\"\n\n[ providers.test ]  # note\ntype = \"openai\"\n").unwrap();
    let error = super::set_search(&path, "tavily", None, Some("tvly-key"));
    assert!(error.as_ref().unwrap_err().to_string().contains("would break the config"), "an appended `[search]` would double the dotted one: {error:?}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "default_model = \"test/model\"\nsearch.provider = \"brave\"\n\n[ providers.test ]  # note\ntype = \"openai\"\n", "a refused edit writes nothing");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn prompt_section_reads_back_as_the_built_in_guidance() {
    let text = format!("{}{}", super::TEMPLATE, super::prompt_section());
    let config: super::Config = toml::from_str(&text).unwrap();
    assert_eq!(config.prompt.system.as_deref(), Some(crate::prompt::DEFAULT));
    assert_eq!(config.guidance(), crate::prompt::DEFAULT);
    let blank: super::Config = toml::from_str("[prompt]\nsystem = \"  \"").unwrap();
    assert_eq!(blank.guidance(), crate::prompt::DEFAULT);
}
