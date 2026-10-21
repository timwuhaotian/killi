use std::path::PathBuf;

use appconfig::cli::config_from_args;
use appconfig::server::Server;
use appconfig::{Config, ConfigError};

fn write(name: &str, text: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("appconfig-{}-{name}.conf", std::process::id()));
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn parses_from_a_string() {
    let config = Config::from_str_contents("host = \"0.0.0.0\" # all\nport = 9000\n\n").unwrap();
    assert_eq!(config, Config { host: "0.0.0.0".into(), port: 9000, workers: 4 });
    assert_eq!(Config::from_str_contents("port 9000"), Err(ConfigError::Syntax(1)));
    assert_eq!(Config::from_str_contents("x = 1"), Err(ConfigError::UnknownKey("x".into())));
    assert_eq!(Config::from_str_contents("workers = many"), Err(ConfigError::BadValue("many".into())));
}

#[test]
fn reads_from_a_path() {
    let path = write("path", "workers = 8\n");
    assert_eq!(Config::from_path(&path).unwrap().workers, 8);
    assert!(matches!(Config::from_path(&PathBuf::from("/nonexistent/x.conf")), Err(ConfigError::Io(_))));
}

#[test]
fn server_and_cli_use_it() {
    let path = write("server", "host = example.test\nport = 81\n");
    assert_eq!(Server::from_config_file(&path).unwrap().address(), "example.test:81");
    let args = vec!["app".to_string(), "--config".to_string(), path.display().to_string()];
    assert_eq!(config_from_args(&args).port, 81);
}
