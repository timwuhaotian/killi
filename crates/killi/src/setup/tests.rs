use super::*;

fn write_skill(root: &Path, name: &str) {
    let dir = root.join(".agents/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Does {name}\n---\nBody of {name}."),
    )
    .unwrap();
}

fn setup(root: &Path, tools: Vec<Value>) -> Setup {
    let config = fresh_config();
    let skills = Skills::discover(root, &config.skills.dirs);
    let prompt = prompt::build(root, &skills, config.guidance());
    let settings = Settings {
        skills: skills.clone(),
        instruction_files: prompt.instruction_files.clone(),
        ..Settings::default()
    };
    Setup {
        config,
        cwd: root.to_path_buf(),
        model: None,
        last: Choice::default(),
        prompt,
        tools,
        settings,
        skills,
        notes: Vec::new(),
        defer_key: false,
    }
}

fn fresh_config() -> Config {
    let mut config = Config::default();
    config.skills.dirs = vec!["/nonexistent".to_string()];
    config
}

#[test]
fn a_keyed_default_is_kept_over_the_first_keyed_model() {
    let config: Config = toml::from_str(
        "default_model = \"openai/gpt\"\n\n[providers.openai]\ntype = \"openai\"\napi_key = \"k\"\n\n[providers.anthropic]\ntype = \"anthropic\"\napi_key = \"k\"\n\n[models.\"openai/gpt\"]\nprovider = \"openai\"\nmodel = \"gpt\"\n\n[models.\"anthropic/claude\"]\nprovider = \"anthropic\"\nmodel = \"claude\"\n",
    )
    .unwrap();
    assert_eq!(
        requested_model(&config, None, &Choice::default(), true).as_deref(),
        Some("openai/gpt"),
        "a keyed default wins over the alphabetically first keyed model"
    );
}

#[test]
fn a_keyless_default_falls_to_the_first_keyed_model() {
    let config: Config = toml::from_str(
        "default_model = \"openai/gpt\"\n\n[providers.openai]\ntype = \"openai\"\napi_key_env = \"KILLI_TEST_ABSENT_KEY\"\n\n[providers.anthropic]\ntype = \"anthropic\"\napi_key = \"k\"\n\n[models.\"openai/gpt\"]\nprovider = \"openai\"\nmodel = \"gpt\"\n\n[models.\"anthropic/claude\"]\nprovider = \"anthropic\"\nmodel = \"claude\"\n",
    )
    .unwrap();
    assert_eq!(
        requested_model(&config, None, &Choice::default(), true).as_deref(),
        Some("anthropic/claude")
    );
}

#[tokio::test]
async fn a_filtered_default_is_not_used_by_a_headless_start() {
    let root = std::env::temp_dir().join(format!("killi-start-filtered-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let previous = std::env::var_os("HOME");
    // SAFETY: the override isolates the saved model choice, then HOME is restored.
    unsafe { std::env::set_var("HOME", &root) };
    let config: Config = toml::from_str(
        "default_model = \"zai/glm-4.5\"\n\n[providers.zai]\ntype = \"openai\"\napi_key = \"k\"\n\n[models.\"zai/glm-4.5\"]\nprovider = \"zai\"\nmodel = \"glm-4.5\"\n\n[models.\"zai/glm-5.3\"]\nprovider = \"zai\"\nmodel = \"glm-5.3\"\n\n[models_filter.zai]\ninclude = [\"glm-5.3\"]\n",
    )
    .unwrap();
    let setup = Setup::new(config, &root, None, false).await.unwrap();
    let agent = setup.start(Controls::default(), Rc::new(|_| {})).unwrap();
    assert_eq!(agent.model_alias(), "zai/glm-5.3");
    match previous {
        Some(home) => unsafe { std::env::set_var("HOME", home) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn an_interactive_start_defers_only_a_missing_key() {
    let root = std::env::temp_dir().join(format!("killi-start-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let source = "default_model = \"t/model\"\n\n[providers.t]\ntype = \"openai\"\napi_key_env = \"KILLI_TEST_ABSENT_KEY\"\n\n[models.\"t/model\"]\nprovider = \"t\"\nmodel = \"model\"\n";
    let config: Config = toml::from_str(source).unwrap();
    let Err(error) = Setup::new(config, &root, Some("t/model"), false).await else {
        panic!("a missing key must end a headless start");
    };
    assert!(error.to_string().contains("KILLI_TEST_ABSENT_KEY"), "{error}");
    let config: Config = toml::from_str(source).unwrap();
    let setup = Setup::new(config, &root, Some("t/model"), true)
        .await
        .unwrap();
    let agent = setup
        .start(Controls::default(), Rc::new(|_| {}))
        .unwrap();
    assert_eq!(agent.model_alias(), "t/model");
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_broken_default_is_reported_not_an_unconfigured_session() {
    let root = std::env::temp_dir().join(format!("killi-start-broken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let config: Config = toml::from_str(
        "default_model = \"t/unknown\"\n\n[providers.t]\ntype = \"openai\"\napi_key = \"k\"\n",
    )
    .unwrap();
    let Err(error) = Setup::new(config, &root, None, true).await else {
        panic!("a broken default must not open an unconfigured session");
    };
    assert!(error.to_string().contains("unknown model"), "{error}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn reload_picks_up_skills_and_instructions_added_mid_session() {
    let root = std::env::temp_dir().join(format!("killi-setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    write_skill(&root, "first");
    let mut setup = setup(&root, vec![crate::skills::definition()]);

    write_skill(&root, "second");
    std::fs::write(root.join("AGENTS.md"), "Always say hello.").unwrap();
    setup.refresh(fresh_config());

    assert!(
        setup.prompt.text.contains("first, second"),
        "{}",
        setup.prompt.text
    );
    assert!(setup.prompt.text.contains("Always say hello."));
    let under_root = |f: &&PathBuf| {
        f.components()
            .any(|c| Some(c.as_os_str()) == root.file_name())
    };
    assert_eq!(
        setup
            .settings
            .instruction_files
            .iter()
            .filter(under_root)
            .count(),
        1,
        "a global ~/.agents/AGENTS.md may add one"
    );
    assert!(
        setup
            .settings
            .skills
            .run(&serde_json::json!({"name": "second"}))
            .text
            .contains("Body of second.")
    );
    assert!(setup.skill_prompt("second", "").is_some());
    assert!(!setup.skills_need_restart());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn search_settings_follow_web_search_config_and_reload() {
    let root = std::env::temp_dir().join(format!("killi-setup-search-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let mut setup = setup(&root, Vec::new());
    assert!(
        setup.settings.search.provider.is_empty(),
        "unset before any pick"
    );

    setup.set_search(crate::config::SearchConfig {
        provider: Some("searxng".into()),
        url: Some("http://localhost:8888".into()),
        ..Default::default()
    });
    assert_eq!(
        setup.settings.search.provider, "searxng",
        "a pick reaches /new through the settings"
    );
    assert_eq!(
        setup.settings.search.url.as_deref(),
        Some("http://localhost:8888")
    );

    let mut config = fresh_config();
    config.search = crate::config::SearchConfig {
        provider: Some("tavily".into()),
        api_key: Some("k".into()),
        ..Default::default()
    };
    setup.refresh(config);
    assert_eq!(
        setup.settings.search.provider, "tavily",
        "/reload re-derives the re-read config"
    );
    assert_eq!(setup.settings.search.api_key.as_deref(), Some("k"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn eager_dedup_removes_non_adjacent_repeats() {
    // Global `eager = ["godot", "blender"]` plus project `eager = ["godot"]`.
    let mut eager = vec![
        "godot".to_string(),
        "blender".to_string(),
        "godot".to_string(),
    ];
    dedupe_eager(&mut eager);
    assert_eq!(
        eager,
        vec!["godot".to_string(), "blender".to_string()],
        "a repeated server is defined twice"
    );
}

#[test]
fn project_trust_is_keyed_by_repo_and_command_lines() {
    let root = std::env::temp_dir().join(format!("killi-trust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("trusted.json");
    assert!(
        load_trusted(&path).is_empty(),
        "a missing store trusts nothing"
    );

    let entries = vec![("t".to_string(), "/bin/serve --port 1".to_string())];
    let fingerprint = trust_fingerprint(&entries);
    assert_eq!(trust_fingerprint(&entries), fingerprint);
    assert_ne!(
        trust_fingerprint(&[("t".to_string(), "/bin/serve --port 2".to_string())]),
        fingerprint,
        "a changed command re-asks"
    );
    assert_ne!(
        trust_fingerprint(&[("u".to_string(), "/bin/serve --port 1".to_string())]),
        fingerprint,
        "a changed name re-asks"
    );

    let key = root.display().to_string();
    save_trusted(&path, &key, &fingerprint).unwrap();
    let store = load_trusted(&path);
    assert!(
        store.get(&key).is_some_and(|known| *known == fingerprint),
        "an approval matches: {store:?}"
    );
    assert!(
        store
            .get(&format!("{key}-other"))
            .is_none_or(|known| *known != fingerprint),
        "another repo is not trusted by it"
    );
    assert!(
        store
            .get(&key)
            .is_none_or(|known| *known != "0000000000000000"),
        "changed commands no longer match"
    );

    std::fs::write(&path, b"{oops").unwrap();
    assert!(
        load_trusted(&path).is_empty(),
        "a corrupt store trusts nothing"
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn resume_runs_on_the_logs_model_with_its_conversation() {
    let root = std::env::temp_dir().join(format!("killi-setup-resume-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let log = root.join("1-1.jsonl");
    let lines = [
        serde_json::json!({"type": "session", "model": "test/model"}),
        serde_json::json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
        serde_json::json!({"type": "message", "message": {"role": "user", "content": "hi"}}),
    ];
    std::fs::write(
        &log,
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
    .unwrap();
    let mut config: Config =
        toml::from_str("default_model = \"test/model\"\n\n[providers.test]\ntype = \"openai\"\napi_key = \"k\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n").unwrap();
    config.skills.dirs = vec!["/nonexistent".to_string()];
    let skills = Skills::discover(&root, &config.skills.dirs);
    let prompt = prompt::build(&root, &skills, config.guidance());
    let settings = Settings {
        skills: skills.clone(),
        instruction_files: prompt.instruction_files.clone(),
        ..Settings::default()
    };
    let setup = Setup {
        config,
        cwd: root.clone(),
        model: None,
        last: Choice::default(),
        prompt,
        tools: Vec::new(),
        settings,
        skills,
        notes: Vec::new(),
        defer_key: false,
    };
    let agent = setup
        .resume(&log, Controls::default(), Rc::new(|_| {}))
        .unwrap();
    assert_eq!(agent.model_alias(), "test/model");
    assert!(
        agent.user_prompts().iter().any(|p| p == "hi"),
        "the log's conversation replays"
    );
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn every_session_gets_the_configured_backup_model() {
    let root = std::env::temp_dir().join(format!("killi-setup-backup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let log = root.join("1-1.jsonl");
    let lines = [
        serde_json::json!({"type": "session", "model": "test/model"}),
        serde_json::json!({"type": "message", "message": {"role": "system", "content": "sys"}}),
        // A record from before the backup moved to the config: the config wins.
        serde_json::json!({"type": "backup", "model": "test/other"}),
    ];
    std::fs::write(
        &log,
        lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
    .unwrap();
    let mut config: Config = toml::from_str(
        "default_model = \"test/model\"\nbackup_model = \"test/backup\"\n\n[providers.test]\ntype = \"openai\"\napi_key = \"k\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n\n[models.\"test/backup\"]\nprovider = \"test\"\nmodel = \"backup\"\n",
    )
    .unwrap();
    config.skills.dirs = vec!["/nonexistent".to_string()];
    let skills = Skills::discover(&root, &config.skills.dirs);
    let prompt = prompt::build(&root, &skills, config.guidance());
    let settings = Settings {
        skills: skills.clone(),
        instruction_files: prompt.instruction_files.clone(),
        ..Settings::default()
    };
    let setup = Setup {
        config,
        cwd: root.clone(),
        model: None,
        last: Choice::default(),
        prompt,
        tools: Vec::new(),
        settings,
        skills,
        notes: Vec::new(),
        defer_key: false,
    };
    let agent = setup
        .resume(&log, Controls::default(), Rc::new(|_| {}))
        .unwrap();
    assert_eq!(agent.model_alias(), "test/model");
    assert_eq!(
        agent.backup_alias(),
        Some("test/backup"),
        "the config's backup_model is what every session starts with"
    );
    drop(agent);

    // A backup the config no longer knows is dropped, not fatal.
    let mut setup = setup;
    setup.set_backup_model(Some("test/gone"));
    let agent = setup
        .resume(&log, Controls::default(), Rc::new(|_| {}))
        .unwrap();
    assert_eq!(
        agent.backup_alias(),
        None,
        "an unknown backup alias starts without one"
    );
    drop(agent);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn reload_without_the_skill_tool_keeps_skills_out_of_the_prompt() {
    let root = std::env::temp_dir().join(format!("killi-setup-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let mut setup = setup(&root, Vec::new());

    write_skill(&root, "late");
    setup.refresh(fresh_config());

    assert!(!setup.prompt.text.contains("# Skills"));
    assert!(setup.skill_prompt("late", "").is_some());
    assert!(setup.skills_need_restart());
    std::fs::remove_dir_all(root).unwrap();
}
