use lazy_static::lazy_static;
use serde::Deserialize;
use toml as serde_toml;
use tracing::warn;

use crate::tools::BashInput;

#[derive(Clone)]
struct SafeCommandRule {
    name: String,
    args: ArgPolicy,
}

#[derive(Clone)]
enum ArgPolicy {
    Any,
    Deny,
}

const SAFE_COMMANDS_TOML: &str = include_str!("safe_commands.toml");

#[derive(Deserialize)]
struct SafeCommandConfig {
    commands: Vec<SafeCommandConfigEntry>,
}

#[derive(Deserialize)]
struct SafeCommandConfigEntry {
    name: String,
    #[serde(default)]
    allow_any: bool,
}

lazy_static! {
    static ref SAFE_COMMAND_RULES: Vec<SafeCommandRule> = load_safe_command_rules();
}

const SHELL_UNSAFE_CHARS: &[char] = &[';', '|', '&', '>', '<', '`', '\n', '\r'];

pub fn should_bypass_permission(input: &BashInput) -> bool {
    is_safe_command(&input.command)
}

fn is_safe_command(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    if trimmed.contains("$(") {
        return false;
    }

    if trimmed.chars().any(|ch| SHELL_UNSAFE_CHARS.contains(&ch)) {
        return false;
    }

    let mut parts = trimmed.split_whitespace();
    let Some(cmd) = parts.next() else {
        return false;
    };

    let args: Vec<&str> = parts.collect();
    let Some(rule) = SAFE_COMMAND_RULES.iter().find(|rule| rule.name == cmd) else {
        return false;
    };
    is_safe_args(&args, &rule.args)
}

fn is_safe_args(_args: &[&str], policy: &ArgPolicy) -> bool {
    if matches!(policy, ArgPolicy::Any) {
        return true;
    }
    false
}

fn load_safe_command_rules() -> Vec<SafeCommandRule> {
    let config: SafeCommandConfig = match serde_toml::from_str(SAFE_COMMANDS_TOML) {
        Ok(config) => config,
        Err(err) => {
            warn!("failed to parse safe_commands.toml: {err}");
            return Vec::new();
        }
    };

    config
        .commands
        .into_iter()
        .map(|entry| {
            if entry.allow_any {
                return SafeCommandRule {
                    name: entry.name,
                    args: ArgPolicy::Any,
                };
            }
            SafeCommandRule {
                name: entry.name,
                args: ArgPolicy::Deny,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_command_allows_simple_invocation() {
        assert!(is_safe_command("ls"));
        assert!(is_safe_command("pwd"));
        assert!(is_safe_command("cat README.md"));
        assert!(is_safe_command("ls -la"));
    }

    #[test]
    fn unsafe_command_rejects_shell_chaining_or_substitution() {
        assert!(!is_safe_command("ls; rm -rf /"));
        assert!(!is_safe_command("cat file | head -n 1"));
        assert!(!is_safe_command("echo $(whoami)"));
        assert!(!is_safe_command("whoami && id"));
        assert!(!is_safe_command("ls > out.txt"));
    }

    #[test]
    fn unsafe_command_rejects_unknown_or_empty() {
        assert!(!is_safe_command(""));
        assert!(!is_safe_command("   "));
        assert!(!is_safe_command("bash -c ls"));
        assert!(!is_safe_command("sudo ls"));
    }
}
