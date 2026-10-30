use super::*;

fn write_skill(root: &Path, name: &str) {
    let dir = root.join(".agents/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), format!("---\nname: {name}\ndescription: Does {name}\n---\nBody of {name}.")).unwrap();
}

fn setup(root: &Path, tools: Vec<Value>) -> Setup {
    let config = fresh_config();
    let skills = Skills::discover(root, &config.skills.dirs);
    let prompt = prompt::build(root, &skills, config.guidance());
    let settings = Settings { skills: skills.clone(), instruction_files: prompt.instruction_files.clone(), ..Settings::default() };
    Setup { config, cwd: root.to_path_buf(), model: None, last: Choice::default(), prompt, tools, settings, skills, notes: Vec::new() }
}

fn fresh_config() -> Config {
    let mut config = Config::default();
    config.skills.dirs = vec!["/nonexistent".to_string()];
    config
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

    assert!(setup.prompt.text.contains("first, second"), "{}", setup.prompt.text);
    assert!(setup.prompt.text.contains("Always say hello."));
    let under_root = |f: &&PathBuf| f.components().any(|c| Some(c.as_os_str()) == root.file_name());
    assert_eq!(setup.settings.instruction_files.iter().filter(under_root).count(), 1, "a global ~/.agents/AGENTS.md may add one");
    assert!(setup.settings.skills.run(&serde_json::json!({"name": "second"})).text.contains("Body of second."));
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
    assert!(setup.settings.search.provider.is_empty(), "unset before any pick");

    setup.set_search(crate::config::SearchConfig { provider: Some("searxng".into()), url: Some("http://localhost:8888".into()), ..Default::default() });
    assert_eq!(setup.settings.search.provider, "searxng", "a pick reaches /new through the settings");
    assert_eq!(setup.settings.search.url.as_deref(), Some("http://localhost:8888"));

    let mut config = fresh_config();
    config.search = crate::config::SearchConfig { provider: Some("tavily".into()), api_key: Some("k".into()), ..Default::default() };
    setup.refresh(config);
    assert_eq!(setup.settings.search.provider, "tavily", "/reload re-derives the re-read config");
    assert_eq!(setup.settings.search.api_key.as_deref(), Some("k"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn eager_dedup_removes_non_adjacent_repeats() {
    // Global `eager = ["godot", "blender"]` plus project `eager = ["godot"]`.
    let mut eager = vec!["godot".to_string(), "blender".to_string(), "godot".to_string()];
    dedupe_eager(&mut eager);
    assert_eq!(eager, vec!["godot".to_string(), "blender".to_string()], "a repeated server is defined twice");
}

#[test]
fn project_trust_is_keyed_by_repo_and_command_lines() {
    let root = std::env::temp_dir().join(format!("killi-trust-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("trusted.json");
    assert!(load_trusted(&path).is_empty(), "a missing store trusts nothing");

    let entries = vec![("t".to_string(), "/bin/serve --port 1".to_string())];
    let fingerprint = trust_fingerprint(&entries);
    assert_eq!(trust_fingerprint(&entries), fingerprint);
    assert_ne!(trust_fingerprint(&[("t".to_string(), "/bin/serve --port 2".to_string())]), fingerprint, "a changed command re-asks");
    assert_ne!(trust_fingerprint(&[("u".to_string(), "/bin/serve --port 1".to_string())]), fingerprint, "a changed name re-asks");

    let key = root.display().to_string();
    save_trusted(&path, &key, &fingerprint).unwrap();
    let store = load_trusted(&path);
    assert!(store.get(&key).is_some_and(|known| *known == fingerprint), "an approval matches: {store:?}");
    assert!(!store.get(&format!("{key}-other")).is_some_and(|known| *known == fingerprint), "another repo is not trusted by it");
    assert!(!store.get(&key).is_some_and(|known| *known == "0000000000000000"), "changed commands no longer match");

    std::fs::write(&path, b"{oops").unwrap();
    assert!(load_trusted(&path).is_empty(), "a corrupt store trusts nothing");
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
    std::fs::write(&log, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    let mut config: Config =
        toml::from_str("default_model = \"test/model\"\n\n[providers.test]\ntype = \"openai\"\napi_key = \"k\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n").unwrap();
    config.skills.dirs = vec!["/nonexistent".to_string()];
    let skills = Skills::discover(&root, &config.skills.dirs);
    let prompt = prompt::build(&root, &skills, config.guidance());
    let settings = Settings { skills: skills.clone(), instruction_files: prompt.instruction_files.clone(), ..Settings::default() };
    let setup = Setup { config, cwd: root.clone(), model: None, last: Choice::default(), prompt, tools: Vec::new(), settings, skills, notes: Vec::new() };
    let agent = setup.resume(&log, Controls::default(), Rc::new(|_| {})).unwrap();
    assert_eq!(agent.model_alias(), "test/model");
    assert!(agent.user_prompts().iter().any(|p| p == "hi"), "the log's conversation replays");
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
    std::fs::write(&log, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    let mut config: Config = toml::from_str(
        "default_model = \"test/model\"\nbackup_model = \"test/backup\"\n\n[providers.test]\ntype = \"openai\"\napi_key = \"k\"\n\n[models.\"test/model\"]\nprovider = \"test\"\nmodel = \"model\"\n\n[models.\"test/backup\"]\nprovider = \"test\"\nmodel = \"backup\"\n",
    )
    .unwrap();
    config.skills.dirs = vec!["/nonexistent".to_string()];
    let skills = Skills::discover(&root, &config.skills.dirs);
    let prompt = prompt::build(&root, &skills, config.guidance());
    let settings = Settings { skills: skills.clone(), instruction_files: prompt.instruction_files.clone(), ..Settings::default() };
    let setup = Setup { config, cwd: root.clone(), model: None, last: Choice::default(), prompt, tools: Vec::new(), settings, skills, notes: Vec::new() };
    let agent = setup.resume(&log, Controls::default(), Rc::new(|_| {})).unwrap();
    assert_eq!(agent.model_alias(), "test/model");
    assert_eq!(agent.backup_alias(), Some("test/backup"), "the config's backup_model is what every session starts with");
    drop(agent);

    // A backup the config no longer knows is dropped, not fatal.
    let mut setup = setup;
    setup.set_backup_model(Some("test/gone"));
    let agent = setup.resume(&log, Controls::default(), Rc::new(|_| {})).unwrap();
    assert_eq!(agent.backup_alias(), None, "an unknown backup alias starts without one");
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
