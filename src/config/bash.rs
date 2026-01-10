use std::{collections::HashMap, path::Path};

use serde::{Deserialize, Serialize};
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SafeCommandsMode {
    #[default]
    Append,
    Override,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BashConfig {
    #[serde(default)]
    pub safe_commands_path: Option<String>,
    #[serde(default)]
    pub safe_commands_mode: SafeCommandsMode,
}

#[derive(Debug, Clone, Default)]
pub struct BashConfigLayers {
    pub global: Option<BashConfig>,
    pub workspace: Option<BashConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SafeCommandEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub allow_any: bool,
    #[serde(default)]
    pub allowed_flags: Vec<String>,
    #[serde(default)]
    pub allow_positional: bool,
    #[serde(default)]
    pub allow_dash: bool,
    #[serde(default)]
    pub flags: Vec<SafeFlagConfig>,
    #[serde(default)]
    pub positional_path_from: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafeFlagConfig {
    pub name: String,
    #[serde(default)]
    pub arg: FlagValuePolicy,
    #[serde(default)]
    pub value: FlagValueType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FlagValuePolicy {
    #[default]
    None,
    Optional,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FlagValueType {
    #[default]
    Any,
    Path,
}

#[derive(Clone)]
pub struct SafeCommandRule {
    pub command_chain: Vec<String>,
    pub args: ArgPolicy,
}

#[derive(Clone)]
pub enum ArgPolicy {
    Any {
        flags: HashMap<String, FlagPolicy>,
        allow_positional: bool,
        positional_path_from: Option<usize>,
        allow_dash: bool,
    },
    AllowList {
        flags: HashMap<String, FlagPolicy>,
        allow_positional: bool,
        positional_path_from: Option<usize>,
        allow_dash: bool,
    },
    Deny,
}

#[derive(Clone, Copy, Debug)]
pub struct FlagPolicy {
    pub arg: FlagValuePolicy,
    pub value_type: FlagValueType,
}

#[derive(Deserialize)]
struct SafeCommandConfig {
    commands: Vec<SafeCommandEntry>,
}

pub fn load_safe_command_rules_from_path(path: &Path) -> Result<Vec<SafeCommandRule>, String> {
    let content =
        std::fs::read_to_string(path).map_err(|err| format!("failed to read file: {err}"))?;
    parse_safe_command_rules(&content)
}

pub fn parse_safe_command_rules(source: &str) -> Result<Vec<SafeCommandRule>, String> {
    let config: SafeCommandConfig =
        toml::from_str(source).map_err(|err| format!("failed to parse config: {err}"))?;
    Ok(build_safe_command_rules_from_entries(config.commands))
}

pub fn build_safe_command_rules_from_entries(
    entries: Vec<SafeCommandEntry>,
) -> Vec<SafeCommandRule> {
    entries
        .into_iter()
        .map(|entry| {
            let mut flags: HashMap<String, FlagPolicy> = HashMap::new();
            for flag in entry.allowed_flags {
                flags.insert(
                    flag,
                    FlagPolicy {
                        arg: FlagValuePolicy::None,
                        value_type: FlagValueType::Any,
                    },
                );
            }
            for flag in entry.flags {
                flags.insert(
                    flag.name,
                    FlagPolicy {
                        arg: flag.arg,
                        value_type: flag.value,
                    },
                );
            }
            let command_chain = match entry.command {
                Some(command) => command,
                None => match entry.name {
                    Some(name) => vec![name],
                    None => {
                        warn!("safe command entry missing name or command");
                        return SafeCommandRule {
                            command_chain: Vec::new(),
                            args: ArgPolicy::Deny,
                        };
                    }
                },
            };
            if command_chain.is_empty() || command_chain.iter().any(|item| item.trim().is_empty()) {
                warn!("safe command entry has empty command chain");
                return SafeCommandRule {
                    command_chain: Vec::new(),
                    args: ArgPolicy::Deny,
                };
            }
            let allow_positional = entry.allow_positional || entry.allow_any;
            let allow_dash = entry.allow_dash;
            if flags.is_empty() && !allow_positional && !allow_dash {
                return SafeCommandRule {
                    command_chain,
                    args: ArgPolicy::Deny,
                };
            }
            if entry.allow_any {
                return SafeCommandRule {
                    command_chain,
                    args: ArgPolicy::Any {
                        flags,
                        allow_positional,
                        positional_path_from: entry.positional_path_from,
                        allow_dash,
                    },
                };
            }
            SafeCommandRule {
                command_chain,
                args: ArgPolicy::AllowList {
                    flags,
                    allow_positional,
                    positional_path_from: entry.positional_path_from,
                    allow_dash,
                },
            }
        })
        .collect()
}
