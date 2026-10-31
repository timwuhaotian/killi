//! Meta's Muse Code from the command line: `killi login muse-code` writes, or repairs, what
//! `/provider muse-code` does, so a machine whose config has no model to start on can begin
//! on Muse Code.

use anyhow::{Result, bail};

use crate::config::{Config, killi_home, muse_ours};

/// Whether the config has Muse Code with a key.
pub(super) fn signed_in() -> bool {
    Config::load().is_ok_and(|config| config.find_provider(muse_ours).is_some_and(|(_, key)| key))
}

/// `killi login muse-code`: takes the pasted key and writes the `meta` provider and its
/// models, or brings the provider the config has up to what Muse Code needs.
pub(super) fn login() -> Result<()> {
    eprintln!("{}", crate::config::MUSE_ABOUT);
    let key = crate::config::read_secret("Paste your Muse Code API key: ")?;
    let key = key.trim();
    if key.is_empty() {
        bail!("no key given, so nothing changed");
    }
    println!("{}", crate::config::add_muse(key)?.report());
    Ok(())
}

/// `killi logout muse-code`: takes the key out of the config.
pub(super) fn logout() -> Result<()> {
    let path = killi_home().join("config.toml");
    let name = Config::load().ok().and_then(|config| {
        config
            .find_provider(muse_ours)
            .map(|(name, _)| name.to_string())
    });
    match name {
        Some(name) if crate::config::remove_provider_key(&path, &name)? => {
            println!("Took the Muse Code key out of {}.", path.display());
        }
        _ => println!("killi has no Muse Code key."),
    }
    Ok(())
}
