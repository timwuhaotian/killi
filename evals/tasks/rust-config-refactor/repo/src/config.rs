use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub workers: usize,
}

#[derive(Debug, PartialEq)]
pub enum ConfigError {
    Io(String),
    /// A line that isn't `key = value`, with its 1-based line number.
    Syntax(usize),
    UnknownKey(String),
    BadValue(String),
}

impl Default for Config {
    fn default() -> Self {
        Self { host: "127.0.0.1".into(), port: 8080, workers: 4 }
    }
}

impl Config {
    /// Reads `key = value` lines; `#` starts a comment. Missing keys keep their defaults.
    pub fn load_from(path: &Path) -> Result<Config, ConfigError> {
        let text = fs::read_to_string(path).map_err(|e| ConfigError::Io(e.to_string()))?;
        let mut config = Config::default();
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(ConfigError::Syntax(n + 1));
            };
            let value = value.trim().trim_matches('"');
            match key.trim() {
                "host" => config.host = value.to_string(),
                "port" => config.port = value.parse().map_err(|_| ConfigError::BadValue(value.to_string()))?,
                "workers" => config.workers = value.parse().map_err(|_| ConfigError::BadValue(value.to_string()))?,
                other => return Err(ConfigError::UnknownKey(other.to_string())),
            }
        }
        Ok(config)
    }
}
