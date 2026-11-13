//! Optional user config: `~/.config/acusage/config.toml` (`$ACUSAGE_CONFIG` overrides).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::pricing::Price;

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
pub struct Config {
    /// Account id or email -> display label.
    #[serde(default)]
    pub aliases: HashMap<String, String>,
    /// Model id -> price override (USD per million tokens).
    #[serde(default)]
    pub prices: HashMap<String, Price>,
}

/// `$ACUSAGE_CONFIG` if set and non-empty, else `<home>/.config/acusage/config.toml`.
pub fn config_path(env_value: Option<&str>, home: &Path) -> PathBuf {
    match env_value.map(str::trim) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home.join(".config").join("acusage").join("config.toml"),
    }
}

/// A missing file gives the defaults; an unreadable or invalid file is an error naming the path.
pub fn load(path: &Path) -> Result<Config> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_env_overrides_home() {
        let home = Path::new("/home/u");
        assert_eq!(
            config_path(Some("/x/c.toml"), home),
            PathBuf::from("/x/c.toml")
        );
        assert_eq!(
            config_path(Some(" "), home),
            PathBuf::from("/home/u/.config/acusage/config.toml")
        );
        assert_eq!(
            config_path(None, home),
            PathBuf::from("/home/u/.config/acusage/config.toml")
        );
    }

    #[test]
    fn missing_file_is_default() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            load(&d.path().join("none.toml")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn parses_aliases_and_prices() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("c.toml");
        std::fs::write(
            &p,
            "[aliases]\n\"user@example.com\" = \"work\"\nabc = \"home\"\n\n\
             [prices.\"gpt-x\"]\ninput = 1.0\noutput = 2.0\ncache_write_5m = 0.0\n\
             cache_write_1h = 0.0\ncache_read = 0.5\n",
        )
        .unwrap();
        let c = load(&p).unwrap();
        assert_eq!(c.aliases["user@example.com"], "work");
        assert_eq!(c.aliases["abc"], "home");
        assert_eq!(c.prices["gpt-x"].output, 2.0);
    }

    #[test]
    fn invalid_file_names_the_path() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("bad.toml");
        std::fs::write(&p, "[aliases\nnope").unwrap();
        let e = format!("{:#}", load(&p).unwrap_err());
        assert!(e.contains("bad.toml"), "{e}");
    }
}
