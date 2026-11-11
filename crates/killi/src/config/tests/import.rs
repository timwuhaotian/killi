use super::*;

#[test]
fn importing_omp_carries_over_providers_models_and_the_default() {
    let root = std::env::temp_dir().join(format!("killi-import-omp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = root.join("omp");
    let dst = root.join("killi");
    std::fs::create_dir_all(&src).unwrap();
    // SAFETY: tests that read this variable don't run concurrently with one that changes it.
    unsafe { std::env::set_var("KILLI_TEST_OMP_KEY", "sk-env") };
    std::fs::write(
        src.join("models.yml"),
        r"# omp's models
providers:
  gateway:
    baseUrl: https://gw.example.com/v1 # the team gateway
    api: openai-completions
    apiKey: sk-gateway
    headers:
      X-Team: platform
    models:
      - id: glm-5.3
        name: GLM 5.3
        reasoning: true
        input: [text, image]
        cost:
          input: 0
          output: 0
        contextWindow: 1000000
        maxTokens: 128000
      - id: fast-chat
        contextWindow: 128000
  envkey:
    baseUrl: https://api.example.com/anthropic
    apiKey: KILLI_TEST_OMP_KEY
    models:
    - id: claude-x
      api: anthropic-messages
      reasoning: true
      maxTokens: 8192
  cmdkey:
    baseUrl: https://cmd.example.com/v1
    apiKey: !op read op://vault/item/key
    models:
      - id: m
  nokey:
    baseUrl: https://x/v1
    models:
      - id: m
",
    )
    .unwrap();
    std::fs::write(
        src.join("config.yml"),
        r"modelRoles:
  default: envkey/claude-x:high, gateway/glm-5.3
",
    )
    .unwrap();
    std::fs::write(
        src.join("mcp.json"),
        serde_json::json!({"mcpServers": {"x": {"command": "echo"}}}).to_string(),
    )
    .unwrap();

    import_omp_from(&src, &dst, false).unwrap();
    let text = std::fs::read_to_string(dst.join("config.toml")).unwrap();
    let config: Config = toml::from_str(&text).unwrap();
    assert_eq!(
        config.default_model.as_deref(),
        Some("envkey/claude-x"),
        "the `default` role, first of the list, with its `:effort` off the end"
    );
    assert_eq!(
        config.models["envkey/claude-x"].default_effort.as_deref(),
        Some("high"),
        "the imported level is the default model's own"
    );
    assert_eq!(
        config.providers.len(),
        2,
        "a `!command` key and a missing key are skipped"
    );
    assert_eq!(
        config.providers["gateway"].api_key.as_deref(),
        Some("sk-gateway")
    );
    assert_eq!(
        config.providers["gateway"].base_url.as_deref(),
        Some("https://gw.example.com/v1"),
        "the comment after the URL drops"
    );
    assert_eq!(
        config.providers["gateway"]
            .custom_headers
            .get("X-Team")
            .map(String::as_str),
        Some("platform")
    );
    assert_eq!(
        config.providers["envkey"].api_key_env.as_deref(),
        Some("KILLI_TEST_OMP_KEY"),
        "a set variable's name reads as `api_key_env`"
    );
    assert_eq!(
        config.providers["envkey"].kind, "anthropic",
        "the model's API names the protocol"
    );
    let glm = &config.models["gateway/glm-5.3"];
    assert_eq!(
        (glm.max_context_size, glm.max_output_size),
        (Some(1_000_000), Some(128_000))
    );
    assert_eq!(glm.capabilities, ["thinking", "image_in"]);
    assert_eq!(
        config.models["gateway/fast-chat"].max_context_size,
        Some(128_000)
    );
    assert_eq!(
        std::fs::read_to_string(dst.join("mcp.json")).unwrap(),
        std::fs::read_to_string(src.join("mcp.json")).unwrap()
    );
    assert!(
        !config.missing_key(None),
        "the imported config starts as it is"
    );
    // SAFETY: as above.
    unsafe { std::env::remove_var("KILLI_TEST_OMP_KEY") };
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn omp_yaml_the_importer_cannot_read_is_reported() {
    let root = std::env::temp_dir().join(format!("killi-import-omp-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("models.yml"),
        "providers:\n  a:\n    apiKey: |\n      sk-1\n",
    )
    .unwrap();
    assert!(
        read_yaml(&root.join("models.yml"))
            .unwrap_err()
            .to_string()
            .contains("unsupported YAML")
    );
    assert!(read_yaml(&root.join("missing.yml")).unwrap().is_none());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn importing_codex_carries_over_custom_providers_the_model_and_mcp() {
    let root = std::env::temp_dir().join(format!("killi-import-codex-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = root.join("codex");
    let dst = root.join("killi");
    std::fs::create_dir_all(&src).unwrap();
    // SAFETY: tests that read this variable don't run concurrently with one that changes it.
    unsafe { std::env::set_var("KILLI_TEST_CODEX_TOKEN", "tok-live") };
    std::fs::write(
        src.join("config.toml"),
        r#"
model = "gpt-5.3-codex"
model_provider = "openai"
model_context_window = 272000
model_reasoning_effort = "high"

[model_providers.gateway]
base_url = "https://gw.example.com/v1"
env_key = "KILLI_TEST_CODEX_KEY"
wire_api = "responses"
http_headers = { X-Team = "platform" }
query_params = { api-version = "v1" }

[model_providers.nokey]
base_url = "https://x/v1"

[model_providers.noplace]
env_key = "KILLI_TEST_CODEX_KEY"

[mcp_servers.files]
command = "npx"
args = ["-y", "files-mcp"]
env = { MCP_TOKEN = "tok-file" }

[mcp_servers.remote]
url = "https://mcp.example.com/mcp"
http_headers = { X-Team = "platform" }
env_http_headers = { Authorization = "KILLI_TEST_CODEX_TOKEN", Missing = "KILLI_TEST_CODEX_ABSENT" }
bearer_token_env_var = "KILLI_TEST_CODEX_BEARER"

[mcp_servers.empty]

[mcp_servers.off]
command = "echo"
enabled = false
"#,
    )
    .unwrap();

    import_codex_from(&src.join("config.toml"), &dst, false).unwrap();
    let text = std::fs::read_to_string(dst.join("config.toml")).unwrap();
    let config: Config = toml::from_str(&text).unwrap();
    assert_eq!(
        config.default_model.as_deref(),
        Some("gateway/gpt-5.3-codex"),
        "a model on codex's own login falls back to the custom provider"
    );
    assert_eq!(
        config.providers.len(),
        1,
        "providers without an endpoint or key variable are skipped"
    );
    let gateway = &config.providers["gateway"];
    assert_eq!(gateway.kind, "openai_responses");
    assert_eq!(
        gateway.base_url.as_deref(),
        Some("https://gw.example.com/v1")
    );
    assert_eq!(gateway.api_key_env.as_deref(), Some("KILLI_TEST_CODEX_KEY"));
    assert_eq!(
        gateway.custom_headers.get("X-Team").map(String::as_str),
        Some("platform")
    );
    let model = &config.models["gateway/gpt-5.3-codex"];
    assert_eq!(model.max_context_size, Some(272000));
    assert_eq!(model.default_effort.as_deref(), Some("high"));
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dst.join("mcp.json")).unwrap()).unwrap();
    let servers = mcp.get("mcpServers").unwrap();
    assert_eq!(
        servers.get("files").unwrap().get("args").unwrap(),
        &serde_json::json!(["-y", "files-mcp"])
    );
    assert_eq!(
        servers
            .get("files")
            .unwrap()
            .get("env")
            .unwrap()
            .get("MCP_TOKEN")
            .unwrap(),
        "tok-file"
    );
    let remote = servers.get("remote").unwrap();
    assert_eq!(
        remote.get("headers").unwrap().get("Authorization").unwrap(),
        "tok-live",
        "a set variable's header is read now"
    );
    assert!(
        remote.get("headers").unwrap().get("Missing").is_none(),
        "an unset variable's header is left out"
    );
    assert_eq!(
        remote.get("bearerTokenEnvVar").unwrap(),
        "KILLI_TEST_CODEX_BEARER"
    );
    assert!(
        servers.get("empty").is_none() && servers.get("off").is_none(),
        "servers with nowhere to run and disabled ones are left out"
    );
    assert!(config.missing_key(None), "the key variable isn't set");

    // Without a configured model the providers still come over, with no default.
    std::fs::write(
        src.join("bare.toml"),
        r#"
[model_providers.gateway]
base_url = "https://gw.example.com/v1"
env_key = "KILLI_TEST_CODEX_KEY"
"#,
    )
    .unwrap();
    let bare = root.join("bare");
    import_codex_from(&src.join("bare.toml"), &bare, false).unwrap();
    let config: Config =
        toml::from_str(&std::fs::read_to_string(bare.join("config.toml")).unwrap()).unwrap();
    assert_eq!(config.providers.len(), 1);
    assert_eq!(config.default_model, None);
    assert!(!bare.join("mcp.json").exists());

    assert!(
        import_codex_from(&src.join("missing.toml"), &root.join("absent"), false)
            .unwrap_err()
            .to_string()
            .contains("no codex config"),
        "a missing file names itself"
    );
    std::fs::write(src.join("oauth.toml"), "model = \"gpt-x\"\n").unwrap();
    assert!(
        import_codex_from(&src.join("oauth.toml"), &root.join("oauth"), false)
            .unwrap_err()
            .to_string()
            .contains("no custom provider"),
        "codex's own login is never imported"
    );
    // SAFETY: as above.
    unsafe { std::env::remove_var("KILLI_TEST_CODEX_TOKEN") };
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn importing_claude_reads_the_key_endpoint_model_and_servers() {
    let root = std::env::temp_dir().join(format!("killi-import-claude-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = root.join("claude");
    let dst = root.join("killi");
    std::fs::create_dir_all(&src).unwrap();
    // Only this test touches these variables; their values come back at the end.
    let saved: Vec<(&str, Option<String>)> =
        ["ANTHROPIC_API_KEY", "ANTHROPIC_BASE_URL", "ANTHROPIC_MODEL"]
            .into_iter()
            .map(|name| (name, std::env::var(name).ok()))
            .collect();
    // SAFETY: as above.
    unsafe {
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("ANTHROPIC_BASE_URL");
        std::env::remove_var("ANTHROPIC_MODEL");
    }

    assert!(
        import_claude_from(&src, &src.join("claude.json"), &dst, false)
            .unwrap_err()
            .to_string()
            .contains("no settings.json"),
        "a missing file names itself"
    );
    std::fs::write(
        src.join("settings.json"),
        serde_json::json!({"model": "claude-opus-4-6"}).to_string(),
    )
    .unwrap();
    assert!(
        import_claude_from(&src, &src.join("claude.json"), &dst, false)
            .unwrap_err()
            .to_string()
            .contains("no API key"),
        "a subscription login bails like a skipped provider"
    );
    std::fs::write(
        src.join("settings.json"),
        serde_json::json!({
            "model": "claude-opus-4-6",
            "env": {
                "ANTHROPIC_API_KEY": "sk-file",
                "ANTHROPIC_BASE_URL": "https://proxy.example.com"
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        src.join("claude.json"),
        serde_json::json!({"mcpServers": {"files": {"command": "files-mcp", "args": ["--root", "/tmp"]}}})
            .to_string(),
    )
    .unwrap();

    import_claude_from(&src, &src.join("claude.json"), &dst, false).unwrap();
    let text = std::fs::read_to_string(dst.join("config.toml")).unwrap();
    let config: Config = toml::from_str(&text).unwrap();
    assert_eq!(
        config.default_model.as_deref(),
        Some("anthropic/claude-opus-4-6")
    );
    let provider = &config.providers["anthropic"];
    assert_eq!(provider.kind, "anthropic");
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://proxy.example.com")
    );
    assert_eq!(
        provider.api_key.as_deref(),
        Some("sk-file"),
        "a key only the settings set is copied inline"
    );
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dst.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(
        mcp.get("mcpServers")
            .unwrap()
            .get("files")
            .unwrap()
            .get("args")
            .unwrap(),
        &serde_json::json!(["--root", "/tmp"])
    );

    // A live key stays a reference, and wins over the file's.
    // SAFETY: as above.
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", "sk-live") };
    let live = root.join("live");
    import_claude_from(&src, &src.join("claude.json"), &live, false).unwrap();
    let config: Config =
        toml::from_str(&std::fs::read_to_string(live.join("config.toml")).unwrap()).unwrap();
    assert_eq!(
        config.providers["anthropic"].api_key_env.as_deref(),
        Some("ANTHROPIC_API_KEY")
    );
    assert_eq!(config.providers["anthropic"].api_key, None);

    // SAFETY: as above.
    unsafe {
        for (name, value) in saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn importing_gemini_needs_an_endpoint_and_reads_the_rest_from_files_or_env() {
    let root = std::env::temp_dir().join(format!("killi-import-gemini-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = root.join("gemini");
    let dst = root.join("killi");
    std::fs::create_dir_all(&src).unwrap();
    // Only this test touches these variables; their values come back at the end.
    let saved: Vec<(&str, Option<String>)> = [
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_GEMINI_BASE_URL",
        "GEMINI_MODEL",
    ]
    .into_iter()
    .map(|name| (name, std::env::var(name).ok()))
    .collect();
    // SAFETY: as above.
    unsafe {
        std::env::remove_var("GEMINI_API_KEY");
        std::env::remove_var("GOOGLE_API_KEY");
        std::env::remove_var("GOOGLE_GEMINI_BASE_URL");
        std::env::remove_var("GEMINI_MODEL");
    }

    assert!(
        import_gemini_from(&src, &dst, false)
            .unwrap_err()
            .to_string()
            .contains("no settings.json"),
        "a missing file names itself"
    );
    std::fs::write(
        src.join("settings.json"),
        serde_json::json!({
            "model": {"name": "gemini-3-pro"},
            "mcpServers": {"files": {"command": "files-mcp"}}
        })
        .to_string(),
    )
    .unwrap();
    assert!(
        import_gemini_from(&src, &dst, false)
            .unwrap_err()
            .to_string()
            .contains("no OpenAI-compatible endpoint"),
        "without a proxy endpoint there is nothing killi can speak to"
    );
    std::fs::write(
        src.join(".env"),
        "# gemini's environment\nexport GOOGLE_GEMINI_BASE_URL=\"https://gw.example.com/v1\"\n",
    )
    .unwrap();
    assert!(
        import_gemini_from(&src, &dst, false)
            .unwrap_err()
            .to_string()
            .contains("no API key"),
        "a Google login bails like a skipped provider"
    );
    std::fs::write(
        src.join(".env"),
        "export GOOGLE_GEMINI_BASE_URL=https://gw.example.com/v1\nGOOGLE_API_KEY='sk-file'\n",
    )
    .unwrap();

    import_gemini_from(&src, &dst, false).unwrap();
    let text = std::fs::read_to_string(dst.join("config.toml")).unwrap();
    let config: Config = toml::from_str(&text).unwrap();
    assert_eq!(config.default_model.as_deref(), Some("gemini/gemini-3-pro"));
    let provider = &config.providers["gemini"];
    assert_eq!(provider.kind, "openai");
    assert_eq!(
        provider.base_url.as_deref(),
        Some("https://gw.example.com/v1")
    );
    assert_eq!(
        provider.api_key.as_deref(),
        Some("sk-file"),
        "a `.env`-only key is copied inline, quotes off"
    );
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dst.join("mcp.json")).unwrap()).unwrap();
    assert!(mcp.get("mcpServers").unwrap().get("files").is_some());

    // Live variables win over the file and stay references.
    // SAFETY: as above.
    unsafe {
        std::env::set_var("GEMINI_API_KEY", "sk-live");
        std::env::set_var("GOOGLE_GEMINI_BASE_URL", "https://live.example.com/v1");
    }
    let live = root.join("live");
    import_gemini_from(&src, &live, false).unwrap();
    let config: Config =
        toml::from_str(&std::fs::read_to_string(live.join("config.toml")).unwrap()).unwrap();
    assert_eq!(
        config.providers["gemini"].api_key_env.as_deref(),
        Some("GEMINI_API_KEY")
    );
    assert_eq!(
        config.providers["gemini"].base_url.as_deref(),
        Some("https://live.example.com/v1")
    );

    // SAFETY: as above.
    unsafe {
        for (name, value) in saved {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
    std::fs::remove_dir_all(&root).unwrap();
}
