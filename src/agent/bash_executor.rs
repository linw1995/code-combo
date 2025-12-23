use std::collections::HashMap;

use lazy_static::lazy_static;
use serde::Deserialize;
use toml as serde_toml;
use tracing::warn;
use tree_sitter::{Node, Parser};

use crate::tools::BashInput;

#[derive(Clone)]
struct SafeCommandRule {
    name: String,
    args: ArgPolicy,
}

#[derive(Clone)]
enum ArgPolicy {
    Any,
    AllowList {
        flags: HashMap<String, FlagValuePolicy>,
        allow_positional: bool,
    },
    Deny,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
enum FlagValuePolicy {
    #[default]
    None,
    Optional,
    Required,
}

#[derive(Debug)]
struct ParsedCommand {
    name: String,
    args: Vec<String>,
}

#[derive(Debug)]
enum ParseError {
    Empty,
    MultipleStatements,
    MissingCommandName,
    ParseFailed,
    SyntaxError,
    UnsupportedNode,
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
    #[serde(default)]
    allowed_flags: Vec<String>,
    #[serde(default)]
    allow_positional: bool,
    #[serde(default)]
    flags: Vec<SafeFlagConfig>,
}

#[derive(Deserialize)]
struct SafeFlagConfig {
    name: String,
    #[serde(default)]
    arg: FlagValuePolicy,
}

lazy_static! {
    static ref SAFE_COMMAND_RULES: Vec<SafeCommandRule> = load_safe_command_rules();
}

pub fn should_bypass_permission(input: &BashInput) -> bool {
    is_safe_command(&input.command)
}

fn is_safe_command(command: &str) -> bool {
    is_safe_command_with_rules(command, &SAFE_COMMAND_RULES)
}

fn is_safe_command_with_rules(command: &str, rules: &[SafeCommandRule]) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    if trimmed.contains('\n') || trimmed.contains('\r') {
        return false;
    }

    let commands = match parse_commands(trimmed) {
        Ok(commands) => commands,
        Err(_) => return false,
    };

    commands.iter().all(|command| {
        let Some(rule) = rules.iter().find(|rule| rule.name == command.name) else {
            return false;
        };
        is_safe_args(&command.args, &rule.args)
    })
}

fn is_safe_args(args: &[String], policy: &ArgPolicy) -> bool {
    match policy {
        ArgPolicy::Any => true,
        ArgPolicy::Deny => args.is_empty(),
        ArgPolicy::AllowList {
            flags,
            allow_positional,
        } => {
            let mut index = 0;
            while index < args.len() {
                let arg = &args[index];
                if arg == "--" {
                    return *allow_positional;
                }

                if is_long_option(arg) {
                    let (name, attached_value) = split_long_option(arg);
                    let Some(policy) = flags.get(name) else {
                        return false;
                    };
                    let consumed = match policy {
                        FlagValuePolicy::None => {
                            if attached_value.is_some() {
                                return false;
                            }
                            1
                        }
                        FlagValuePolicy::Required => {
                            if attached_value.is_some() {
                                1
                            } else if index + 1 < args.len() {
                                2
                            } else {
                                return false;
                            }
                        }
                        FlagValuePolicy::Optional => {
                            if attached_value.is_some() {
                                1
                            } else if index + 1 < args.len() && !is_flag_like(&args[index + 1]) {
                                2
                            } else {
                                1
                            }
                        }
                    };
                    index += consumed;
                    continue;
                }

                if is_short_option(arg) {
                    let consumed = match parse_short_options(arg, index, args, flags) {
                        Some(consumed) => consumed,
                        None => return false,
                    };
                    index += consumed;
                    continue;
                }

                if !*allow_positional {
                    return false;
                }
                index += 1;
            }
            true
        }
    }
}

fn is_flag_like(arg: &str) -> bool {
    arg.starts_with('-') && arg != "-"
}

fn is_long_option(arg: &str) -> bool {
    arg.starts_with("--") && arg.len() > 2
}

fn is_short_option(arg: &str) -> bool {
    arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 1
}

fn split_long_option(arg: &str) -> (&str, Option<&str>) {
    if let Some((name, value)) = arg.split_once('=') {
        (name, Some(value))
    } else {
        (arg, None)
    }
}

fn parse_short_options(
    token: &str,
    index: usize,
    args: &[String],
    flags: &HashMap<String, FlagValuePolicy>,
) -> Option<usize> {
    let body = token.strip_prefix('-')?;
    let mut offset = 0;
    while offset < body.len() {
        let mut iter = body[offset..].chars();
        let ch = iter.next()?;
        let ch_len = ch.len_utf8();
        let name = format!("-{ch}");
        let policy = flags.get(&name)?;
        let remainder = &body[offset + ch_len..];
        match policy {
            FlagValuePolicy::None => {
                if remainder.starts_with('=') {
                    return None;
                }
                offset += ch_len;
            }
            FlagValuePolicy::Required => {
                if remainder.starts_with('=') {
                    return Some(1);
                }
                if !remainder.is_empty() {
                    return Some(1);
                }
                if index + 1 < args.len() {
                    return Some(2);
                }
                return None;
            }
            FlagValuePolicy::Optional => {
                if remainder.starts_with('=') {
                    return Some(1);
                }
                if !remainder.is_empty() {
                    return Some(1);
                }
                if index + 1 < args.len() && !is_flag_like(&args[index + 1]) {
                    return Some(2);
                }
                return Some(1);
            }
        }
    }
    Some(1)
}

fn parse_commands(command: &str) -> Result<Vec<ParsedCommand>, ParseError> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .map_err(|_| ParseError::ParseFailed)?;
    let tree = parser.parse(command, None).ok_or(ParseError::ParseFailed)?;
    let root = tree.root_node();
    if root.has_error() || root.is_missing() {
        return Err(ParseError::SyntaxError);
    }

    if contains_disallowed_tokens(root) {
        return Err(ParseError::UnsupportedNode);
    }

    if root.named_child_count() != 1 {
        return Err(ParseError::MultipleStatements);
    }

    let mut commands = Vec::new();
    collect_commands(root, command, &mut commands)?;
    if commands.is_empty() {
        return Err(ParseError::Empty);
    }

    Ok(commands)
}

fn collect_commands(
    node: Node<'_>,
    source: &str,
    commands: &mut Vec<ParsedCommand>,
) -> Result<(), ParseError> {
    if node.is_error() || node.is_missing() {
        return Err(ParseError::SyntaxError);
    }

    if is_disallowed_node_kind(node.kind()) {
        return Err(ParseError::UnsupportedNode);
    }

    if node.kind() == "command" {
        commands.push(parse_command_node(node, source)?);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_commands(child, source, commands)?;
    }

    Ok(())
}

fn parse_command_node(node: Node<'_>, source: &str) -> Result<ParsedCommand, ParseError> {
    let Some(name_node) = node.child_by_field_name("name") else {
        return Err(ParseError::MissingCommandName);
    };
    let Some(name) = node_text(name_node, source) else {
        return Err(ParseError::MissingCommandName);
    };

    let mut args = Vec::new();
    let mut cursor = node.walk();
    for arg_node in node.children_by_field_name("argument", &mut cursor) {
        if let Some(arg) = node_text(arg_node, source) {
            args.push(arg);
        }
    }

    Ok(ParsedCommand { name, args })
}

fn node_text(node: Node<'_>, source: &str) -> Option<String> {
    node.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

fn is_disallowed_node_kind(kind: &str) -> bool {
    matches!(
        kind,
        "command_substitution"
            | "process_substitution"
            | "redirected_statement"
            | "file_redirect"
            | "heredoc_redirect"
            | "herestring_redirect"
            | "variable_assignment"
            | "variable_assignments"
            | "declaration_command"
            | "unset_command"
            | "test_command"
            | "subshell"
            | "compound_statement"
            | "function_definition"
            | "for_statement"
            | "c_style_for_statement"
            | "while_statement"
            | "if_statement"
            | "case_statement"
            | "negated_command"
    )
}

fn contains_disallowed_tokens(node: Node<'_>) -> bool {
    if is_disallowed_token_kind(node.kind()) {
        return true;
    }

    let mut index = 0;
    let count = node.child_count();
    while index < count {
        if let Some(child) = node.child(index)
            && contains_disallowed_tokens(child)
        {
            return true;
        }
        index += 1;
    }

    false
}

fn is_disallowed_token_kind(kind: &str) -> bool {
    matches!(kind, "&" | ";")
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
            let mut flags: HashMap<String, FlagValuePolicy> = HashMap::new();
            for flag in entry.allowed_flags {
                flags.insert(flag, FlagValuePolicy::None);
            }
            for flag in entry.flags {
                flags.insert(flag.name, flag.arg);
            }
            if flags.is_empty() && !entry.allow_positional {
                return SafeCommandRule {
                    name: entry.name,
                    args: ArgPolicy::Deny,
                };
            }
            SafeCommandRule {
                name: entry.name,
                args: ArgPolicy::AllowList {
                    flags,
                    allow_positional: entry.allow_positional,
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! assert_safe_cmd {
        ($cmd:expr) => {
            assert!(
                is_safe_command($cmd),
                "expected command to be safe: {cmd}",
                cmd = $cmd
            );
        };
    }

    macro_rules! assert_unsafe_cmd {
        ($cmd:expr) => {
            assert!(
                !is_safe_command($cmd),
                "expected command to be unsafe: {cmd}",
                cmd = $cmd
            );
        };
    }

    macro_rules! assert_safe_cmd_with_rules {
        ($cmd:expr, $rules:expr) => {
            assert!(
                is_safe_command_with_rules($cmd, $rules),
                "expected command to be safe: {cmd}",
                cmd = $cmd
            );
        };
    }

    macro_rules! assert_unsafe_cmd_with_rules {
        ($cmd:expr, $rules:expr) => {
            assert!(
                !is_safe_command_with_rules($cmd, $rules),
                "expected command to be unsafe: {cmd}",
                cmd = $cmd
            );
        };
    }

    #[test]
    fn safe_command_allows_simple_invocation() {
        assert_safe_cmd!("ls");
        assert_safe_cmd!("pwd");
        assert_safe_cmd!("cat README.md");
        assert_safe_cmd!("ls -la");
    }

    #[test]
    fn safe_command_allows_pipelines_and_lists() {
        assert_safe_cmd!("cat file | head -n 1");
        assert_safe_cmd!("whoami && id");
        assert_safe_cmd!("false || true");
    }

    #[test]
    fn safe_command_enforces_allowlist_flags() {
        let mut ls_flags = HashMap::new();
        ls_flags.insert("-l".to_string(), FlagValuePolicy::None);
        ls_flags.insert("-a".to_string(), FlagValuePolicy::None);
        ls_flags.insert("--color".to_string(), FlagValuePolicy::Optional);
        let mut head_flags = HashMap::new();
        head_flags.insert("-n".to_string(), FlagValuePolicy::Required);
        let rules = vec![
            SafeCommandRule {
                name: "ls".to_string(),
                args: ArgPolicy::AllowList {
                    flags: ls_flags,
                    allow_positional: false,
                },
            },
            SafeCommandRule {
                name: "head".to_string(),
                args: ArgPolicy::AllowList {
                    flags: head_flags,
                    allow_positional: false,
                },
            },
        ];

        assert_safe_cmd_with_rules!("ls -la", &rules);
        assert_safe_cmd_with_rules!("ls --color=auto", &rules);
        assert_safe_cmd_with_rules!("ls --color auto", &rules);
        assert_unsafe_cmd_with_rules!("ls -a=1", &rules);
        assert_unsafe_cmd_with_rules!("ls -z", &rules);
        assert_unsafe_cmd_with_rules!("ls file.txt", &rules);
        assert_safe_cmd_with_rules!("head -n 10", &rules);
        assert_safe_cmd_with_rules!("head -n10", &rules);
        assert_unsafe_cmd_with_rules!("head -n", &rules);
    }

    #[test]
    fn unsafe_command_rejects_shell_chaining_or_substitution() {
        assert_unsafe_cmd!("ls; rm -rf /");
        assert_unsafe_cmd!("ls &");
        assert_unsafe_cmd!("echo $(whoami)");
        assert_unsafe_cmd!("ls > out.txt");
    }

    #[test]
    fn unsafe_command_rejects_unknown_or_empty() {
        assert_unsafe_cmd!("");
        assert_unsafe_cmd!("   ");
        assert_unsafe_cmd!("bash -c ls");
        assert_unsafe_cmd!("sudo ls");
    }
}
