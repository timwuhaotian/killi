use std::path::Path;

use crate::config::{Config, ConfigError};

pub struct Server {
    pub config: Config,
}

impl Server {
    pub fn from_config_file(path: &Path) -> Result<Server, ConfigError> {
        Ok(Server { config: Config::load_from(path)? })
    }

    pub fn address(&self) -> String {
        format!("{}:{}", self.config.host, self.config.port)
    }
}
