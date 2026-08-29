use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

pub fn default_config_path() -> Option<PathBuf> {
    if let Ok(config_home) = std::env::var("XDG_CONFIG_HOME") {
        Some(
            PathBuf::from(config_home)
                .join("ttyman")
                .join("config.toml"),
        )
    } else if let Ok(home) = std::env::var("HOME") {
        Some(
            PathBuf::from(home)
                .join(".config")
                .join("ttyman")
                .join("config.toml"),
        )
    } else {
        None
    }
}

fn default_menu_key() -> u8 {
    0x1D
}

fn default_menu_command() -> String {
    "echo detach".to_string()
}

#[derive(Deserialize, Debug, Clone)]
pub struct MenuConfig {
    #[serde(default = "default_menu_key")]
    pub key: u8,
    #[serde(default = "default_menu_command")]
    pub command: String,
}

impl Default for MenuConfig {
    fn default() -> Self {
        Self {
            key: default_menu_key(),
            command: default_menu_command(),
        }
    }
}

fn default_scrollback() -> usize {
    10_000
}

#[derive(Deserialize, Debug, Clone)]
pub struct SessionConfig {
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            scrollback: default_scrollback(),
        }
    }
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct Config {
    #[serde(default)]
    pub menu: MenuConfig,
    #[serde(default)]
    pub session: SessionConfig,
}

impl Config {
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let content = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn load_default_or_explicit(config_arg: Option<&str>) -> Option<Self> {
        if let Some(path) = config_arg {
            Self::load_from_file(path).ok()
        } else if let Some(def_path) = default_config_path() {
            if def_path.exists() {
                Self::load_from_file(&def_path).ok()
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn menu_key(&self) -> u8 {
        self.menu.key
    }

    pub fn menu_command(&self) -> &str {
        &self.menu.command
    }

    pub fn scrollback(&self) -> usize {
        if self.session.scrollback > 0 {
            self.session.scrollback
        } else {
            10_000
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_parsing_hex() {
        let toml = r#"
[menu]
key = 0x1D
command = "echo hello"

[session]
scrollback = 5000
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.menu_key(), 0x1D);
        assert_eq!(config.menu_command(), "echo hello");
        assert_eq!(config.scrollback(), 5000);
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.menu_key(), 0x1D);
        assert_eq!(config.menu_command(), "echo detach");
        assert_eq!(config.scrollback(), 10_000);
    }
}
