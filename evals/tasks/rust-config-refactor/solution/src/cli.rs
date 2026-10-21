use std::path::PathBuf;

use crate::config::Config;

/// Loads the config named by `--config <path>`, or the defaults.
pub fn config_from_args(args: &[String]) -> Config {
    match args.iter().position(|a| a == "--config") {
        Some(i) => {
            let path = PathBuf::from(&args[i + 1]);
            Config::from_path(&path).unwrap_or_default()
        }
        None => Config::default(),
    }
}
