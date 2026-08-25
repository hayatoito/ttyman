use serde::Deserialize;
use std::collections::HashMap;
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

pub fn resolve_initial_config(
    config_arg: Option<&str>,
) -> anyhow::Result<(Option<InputRemapper>, Option<String>)> {
    if let Some(path) = config_arg {
        let remapper = InputRemapper::load_from_file(path)?;
        Ok((Some(remapper), Some(path.to_string())))
    } else if let Some(def_path) = default_config_path() {
        if def_path.exists() {
            let remapper = InputRemapper::load_from_file(&def_path)?;
            Ok((Some(remapper), Some(def_path.to_string_lossy().to_string())))
        } else {
            Ok((None, None))
        }
    } else {
        Ok((None, None))
    }
}

#[derive(Debug, Clone, Default)]
struct TrieNode {
    children: HashMap<u8, TrieNode>,
    output: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Default)]
pub struct InputRemapper {
    root: TrieNode,
    pending: Vec<u8>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RemapRule {
    pub from: Vec<u8>,
    pub to: Vec<u8>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct MenuConfig {
    pub key: Option<String>,
    pub command: Option<String>,
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
        Self { scrollback: 10_000 }
    }
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct Config {
    #[serde(default)]
    pub menu: MenuConfig,
    #[serde(default)]
    pub remap: Vec<RemapRule>,
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

    pub fn menu_key(&self) -> String {
        self.menu.key.clone().unwrap_or_else(|| "0x1d".to_string())
    }

    pub fn menu_command(&self) -> String {
        self.menu
            .command
            .clone()
            .unwrap_or_else(|| "echo detach".to_string())
    }

    pub fn scrollback(&self) -> usize {
        if self.session.scrollback > 0 {
            self.session.scrollback
        } else {
            10_000
        }
    }

    pub fn to_remapper(&self) -> Option<InputRemapper> {
        if self.remap.is_empty() {
            None
        } else {
            let mut remapper = InputRemapper::new();
            for rule in &self.remap {
                remapper.insert(&rule.from, &rule.to);
            }
            Some(remapper)
        }
    }
}

impl InputRemapper {
    pub fn new() -> Self {
        Self {
            root: TrieNode::default(),
            pending: Vec::new(),
        }
    }

    pub fn insert(&mut self, trigger: &[u8], target: &[u8]) {
        if trigger.is_empty() {
            return;
        }
        let mut node = &mut self.root;
        for &byte in trigger {
            node = node.children.entry(byte).or_default();
        }
        node.output = Some(target.to_vec());
    }

    pub fn is_empty(&self) -> bool {
        self.root.children.is_empty()
    }

    pub fn count_rules(&self) -> usize {
        fn count(node: &TrieNode) -> usize {
            let self_count = if node.output.is_some() { 1 } else { 0 };
            let child_count: usize = node.children.values().map(count).sum();
            self_count + child_count
        }
        count(&self.root)
    }

    pub fn load_from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let content = fs::read_to_string(path)?;
        Self::from_toml_str(&content)
    }

    pub fn from_toml_str(content: &str) -> anyhow::Result<Self> {
        let config: Config = toml::from_str(content)?;
        Ok(config.to_remapper().unwrap_or_default())
    }

    /// Translates incoming raw input bytes using stateful longest-prefix Trie matching.
    ///
    /// If input bytes form a partial prefix of a defined multi-stroke mapping,
    /// those bytes are held in the internal `pending` buffer and an empty slice is returned
    /// until subsequent bytes either complete the chord or mismatch (causing a flush).
    pub fn translate(&mut self, input: &[u8]) -> Vec<u8> {
        if self.is_empty() {
            return input.to_vec();
        }

        self.pending.extend_from_slice(input);
        let mut output = Vec::new();
        let mut i = 0;

        while i < self.pending.len() {
            let mut curr = &self.root;
            let mut last_match_end = None;
            let mut last_match_output = None;
            let mut is_partial_prefix = false;

            for j in i..self.pending.len() {
                if let Some(next) = curr.children.get(&self.pending[j]) {
                    curr = next;
                    if let Some(ref out) = curr.output {
                        last_match_end = Some(j + 1);
                        last_match_output = Some(out.clone());
                    }
                    if j == self.pending.len() - 1 && !curr.children.is_empty() {
                        is_partial_prefix = true;
                    }
                } else {
                    is_partial_prefix = false;
                    break;
                }
            }

            // If the tail of pending buffer is a partial prefix of a longer rule,
            // hold those remaining bytes in pending buffer.
            if is_partial_prefix && (last_match_end.is_none() || !curr.children.is_empty()) {
                self.pending.drain(0..i);
                return output;
            }

            if let (Some(end), Some(out)) = (last_match_end, last_match_output) {
                output.extend_from_slice(&out);
                i = end;
            } else {
                output.push(self.pending[i]);
                i += 1;
            }
        }

        self.pending.clear();
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_byte_mapping() {
        let toml = r#"
[[remap]]
from = [0x02]
to   = [0x1b, 0x5b, 0x44]

[[remap]]
from = [0x06]
to   = [0x1b, 0x5b, 0x43]
"#;
        let mut remapper = InputRemapper::from_toml_str(toml).unwrap();

        // 0x02 (Ctrl-b) -> \x1b[D (Left)
        assert_eq!(remapper.translate(&[0x02]), b"\x1b[D");

        // 0x06 (Ctrl-f) -> \x1b[C (Right)
        assert_eq!(remapper.translate(&[0x06]), b"\x1b[C");

        // Normal characters remain untouched
        assert_eq!(remapper.translate(b"hello"), b"hello");

        // Mixed sequence
        assert_eq!(remapper.translate(&[b'a', 0x02, b'z']), b"a\x1b[Dz");
    }

    #[test]
    fn test_multistroke_emacs_chord_matching() {
        let toml = r#"
# C-x C-f (0x18, 0x06) -> [0xAA, 0xBB]
[[remap]]
from = [0x18, 0x06]
to   = [0xAA, 0xBB]
"#;
        let mut remapper = InputRemapper::from_toml_str(toml).unwrap();

        // 1st stroke: C-x (0x18) is typed -> held in pending, returns empty!
        let out1 = remapper.translate(&[0x18]);
        assert_eq!(out1, b"");

        // 2nd stroke: C-f (0x06) is typed -> matches chord, returns [0xAA, 0xBB]!
        let out2 = remapper.translate(&[0x06]);
        assert_eq!(out2, &[0xAA, 0xBB]);
    }

    #[test]
    fn test_multistroke_mismatch_flushes_all_keys() {
        let toml = r#"
# C-x C-f (0x18, 0x06) -> [0xAA, 0xBB]
[[remap]]
from = [0x18, 0x06]
to   = [0xAA, 0xBB]
"#;
        let mut remapper = InputRemapper::from_toml_str(toml).unwrap();

        // 1st stroke: C-x (0x18) is typed -> held in pending
        assert_eq!(remapper.translate(&[0x18]), b"");

        // 2nd stroke: 'a' (0x61) is typed (mismatch) -> flushes [0x18, 'a']!
        assert_eq!(remapper.translate(b"a"), &[0x18, b'a']);
    }

    #[test]
    fn test_multibyte_in_single_packet() {
        let toml = r#"
# Alt-f (ESC + f) -> Ctrl-Right
[[remap]]
from = [0x1b, 0x66]
to   = [0x1b, 0x5b, 0x31, 0x3b, 0x35, 0x43]
"#;
        let mut remapper = InputRemapper::from_toml_str(toml).unwrap();

        // Alt-f delivered in single packet
        assert_eq!(remapper.translate(&[0x1b, 0x66]), b"\x1b[1;5C");
    }

    #[test]
    fn test_resolve_initial_config_explicit_and_missing_default() {
        // When config_arg is None and default file doesn't exist, returns (None, None)
        let (remapper, path) = resolve_initial_config(None).unwrap();
        // (Assuming standard test environment without existing ~/.config/ttyman/config.toml, or if it does exist, returns valid)
        if path.is_none() {
            assert!(remapper.is_none());
        }

        // When explicit file is specified, loads that file
        let temp_dir = std::env::temp_dir();
        let test_config_file = temp_dir.join("test_ttyman_remap.toml");
        std::fs::write(&test_config_file, "[[remap]]\nfrom = [0x01]\nto = [0x02]\n").unwrap();

        let (remapper, loaded_path) =
            resolve_initial_config(Some(test_config_file.to_str().unwrap())).unwrap();
        assert!(remapper.is_some());
        assert_eq!(
            loaded_path,
            Some(test_config_file.to_str().unwrap().to_string())
        );

        let _ = std::fs::remove_file(test_config_file);
    }

    #[test]
    fn test_config_tables_parsing() {
        let toml = r#"
[menu]
key = "0x1b"
command = "echo hello"

[[remap]]
from = [0x01]
to = [0x02]

[session]
scrollback = 25000
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.menu_key(), "0x1b");
        assert_eq!(config.menu_command(), "echo hello");
        assert_eq!(config.scrollback(), 25000);
        let mut rm = config.to_remapper().unwrap();
        assert_eq!(rm.translate(&[0x01]), &[0x02]);
    }
}
