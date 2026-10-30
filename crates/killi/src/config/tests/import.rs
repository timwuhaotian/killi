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
        config.missing_key(None).is_none(),
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
