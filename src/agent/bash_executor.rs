use std::collections::HashMap;
use std::path::Path;

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
    Any {
        flags: HashMap<String, FlagPolicy>,
        allow_positional: bool,
        positional_path_from: Option<usize>,
    },
    AllowList {
        flags: HashMap<String, FlagPolicy>,
        allow_positional: bool,
        positional_path_from: Option<usize>,
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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
enum FlagValueType {
    #[default]
    Any,
    Path,
}

#[derive(Clone, Copy, Debug)]
struct FlagPolicy {
    arg: FlagValuePolicy,
    value_type: FlagValueType,
}

#[derive(Debug)]
struct ParsedArg {
    text: String,
    has_expansion: bool,
}

#[derive(Debug)]
struct ParsedCommand {
    name: String,
    args: Vec<ParsedArg>,
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
    #[serde(default)]
    positional_path_from: Option<usize>,
}

#[derive(Deserialize)]
struct SafeFlagConfig {
    name: String,
    #[serde(default)]
    arg: FlagValuePolicy,
    #[serde(default)]
    value: FlagValueType,
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

fn is_safe_args(args: &[ParsedArg], policy: &ArgPolicy) -> bool {
    match policy {
        ArgPolicy::Any {
            flags,
            allow_positional,
            positional_path_from,
        } => is_safe_args_any(args, flags, *allow_positional, *positional_path_from),
        ArgPolicy::Deny => args.is_empty(),
        ArgPolicy::AllowList {
            flags,
            allow_positional,
            positional_path_from,
        } => is_safe_args_allowlist(args, flags, *allow_positional, *positional_path_from),
    }
}

fn is_safe_args_allowlist(
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    allow_positional: bool,
    positional_path_from: Option<usize>,
) -> bool {
    let mut index = 0;
    let mut positional_index = 0;
    let mut options_ended = false;
    while index < args.len() {
        let arg = &args[index];
        let text = arg.text.as_str();

        if !options_ended && text == "--" {
            if !allow_positional {
                return false;
            }
            options_ended = true;
            index += 1;
            continue;
        }

        if !options_ended && is_long_option(text) {
            let (name, attached_value) = split_long_option(text);
            let Some(policy) = flags.get(name) else {
                return false;
            };
            let consumed = match policy.arg {
                FlagValuePolicy::None => {
                    if attached_value.is_some() {
                        return false;
                    }
                    1
                }
                FlagValuePolicy::Required => {
                    if let Some(value) = attached_value {
                        if value.is_empty() {
                            return false;
                        }
                        if policy.value_type == FlagValueType::Path
                            && !is_relative_path_value(value, arg)
                        {
                            return false;
                        }
                        1
                    } else if index + 1 < args.len() {
                        if policy.value_type == FlagValueType::Path
                            && !is_relative_path_arg(&args[index + 1])
                        {
                            return false;
                        }
                        2
                    } else {
                        return false;
                    }
                }
                FlagValuePolicy::Optional => {
                    if let Some(value) = attached_value {
                        if value.is_empty() {
                            return false;
                        }
                        if policy.value_type == FlagValueType::Path
                            && !is_relative_path_value(value, arg)
                        {
                            return false;
                        }
                        1
                    } else if index + 1 < args.len() && !is_flag_like(args[index + 1].text.as_str())
                    {
                        if policy.value_type == FlagValueType::Path
                            && !is_relative_path_arg(&args[index + 1])
                        {
                            return false;
                        }
                        2
                    } else {
                        1
                    }
                }
            };
            index += consumed;
            continue;
        }

        if !options_ended && is_short_option(text) {
            let consumed = match parse_short_options_allowlist(arg, index, args, flags) {
                Some(consumed) => consumed,
                None => return false,
            };
            index += consumed;
            continue;
        }

        if !allow_positional {
            return false;
        }
        if positional_path_from
            .map(|from| positional_index >= from)
            .unwrap_or(false)
            && !is_relative_path_arg(arg)
        {
            return false;
        }
        positional_index += 1;
        index += 1;
    }
    true
}

fn is_safe_args_any(
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    allow_positional: bool,
    positional_path_from: Option<usize>,
) -> bool {
    let mut index = 0;
    let mut positional_index = 0;
    let mut options_ended = false;
    while index < args.len() {
        let arg = &args[index];
        let text = arg.text.as_str();

        if !options_ended && text == "--" {
            if !allow_positional {
                return false;
            }
            options_ended = true;
            index += 1;
            continue;
        }

        if !options_ended && is_long_option(text) {
            let (name, attached_value) = split_long_option(text);
            if let Some(policy) = flags.get(name) {
                let consumed = match policy.arg {
                    FlagValuePolicy::None => {
                        if let Some(value) = attached_value
                            && policy.value_type == FlagValueType::Path
                            && !is_relative_path_value(value, arg)
                        {
                            return false;
                        }
                        1
                    }
                    FlagValuePolicy::Required => {
                        if let Some(value) = attached_value {
                            if value.is_empty() {
                                return false;
                            }
                            if policy.value_type == FlagValueType::Path
                                && !is_relative_path_value(value, arg)
                            {
                                return false;
                            }
                            1
                        } else if index + 1 < args.len()
                            && !is_flag_like(args[index + 1].text.as_str())
                        {
                            if policy.value_type == FlagValueType::Path
                                && !is_relative_path_arg(&args[index + 1])
                            {
                                return false;
                            }
                            2
                        } else {
                            1
                        }
                    }
                    FlagValuePolicy::Optional => {
                        if let Some(value) = attached_value {
                            if value.is_empty() {
                                return false;
                            }
                            if policy.value_type == FlagValueType::Path
                                && !is_relative_path_value(value, arg)
                            {
                                return false;
                            }
                            1
                        } else if index + 1 < args.len()
                            && !is_flag_like(args[index + 1].text.as_str())
                        {
                            if policy.value_type == FlagValueType::Path
                                && !is_relative_path_arg(&args[index + 1])
                            {
                                return false;
                            }
                            2
                        } else {
                            1
                        }
                    }
                };
                index += consumed;
                continue;
            }
            index += 1;
            continue;
        }

        if !options_ended && is_short_option(text) {
            let consumed = match parse_short_options_any(arg, index, args, flags) {
                Some(consumed) => consumed,
                None => return false,
            };
            index += consumed;
            continue;
        }

        if !allow_positional {
            return false;
        }
        if positional_path_from
            .map(|from| positional_index >= from)
            .unwrap_or(false)
            && !is_relative_path_arg(arg)
        {
            return false;
        }
        positional_index += 1;
        index += 1;
    }
    true
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

fn parse_short_options_allowlist(
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
) -> Option<usize> {
    let body = token.text.strip_prefix('-')?;
    let mut offset = 0;
    while offset < body.len() {
        let mut iter = body[offset..].chars();
        let ch = iter.next()?;
        let ch_len = ch.len_utf8();
        let name = format!("-{ch}");
        let policy = flags.get(&name)?;
        let remainder = &body[offset + ch_len..];
        match policy.arg {
            FlagValuePolicy::None => {
                if remainder.starts_with('=') {
                    return None;
                }
                offset += ch_len;
            }
            FlagValuePolicy::Required => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if value.is_empty() {
                        return None;
                    }
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(value, token)
                    {
                        return None;
                    }
                    return Some(1);
                }
                if !remainder.is_empty() {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(remainder, token)
                    {
                        return None;
                    }
                    return Some(1);
                }
                if index + 1 < args.len() {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_arg(&args[index + 1])
                    {
                        return None;
                    }
                    return Some(2);
                }
                return None;
            }
            FlagValuePolicy::Optional => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if value.is_empty() {
                        return None;
                    }
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(value, token)
                    {
                        return None;
                    }
                    return Some(1);
                }
                if !remainder.is_empty() {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(remainder, token)
                    {
                        return None;
                    }
                    return Some(1);
                }
                if index + 1 < args.len() && !is_flag_like(args[index + 1].text.as_str()) {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_arg(&args[index + 1])
                    {
                        return None;
                    }
                    return Some(2);
                }
                return Some(1);
            }
        }
    }
    Some(1)
}

fn parse_short_options_any(
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
) -> Option<usize> {
    let body = token.text.strip_prefix('-')?;
    let mut offset = 0;
    while offset < body.len() {
        let mut iter = body[offset..].chars();
        let ch = iter.next()?;
        let ch_len = ch.len_utf8();
        let name = format!("-{ch}");
        let policy = flags.get(&name);
        let remainder = &body[offset + ch_len..];
        let Some(policy) = policy else {
            if remainder.starts_with('=') {
                return Some(1);
            }
            offset += ch_len;
            continue;
        };
        match policy.arg {
            FlagValuePolicy::None => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(value, token)
                    {
                        return None;
                    }
                    return Some(1);
                }
                offset += ch_len;
            }
            FlagValuePolicy::Required => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if value.is_empty() {
                        return None;
                    }
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(value, token)
                    {
                        return None;
                    }
                    return Some(1);
                }
                if !remainder.is_empty() {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(remainder, token)
                    {
                        return None;
                    }
                    return Some(1);
                }
                if index + 1 < args.len() && !is_flag_like(args[index + 1].text.as_str()) {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_arg(&args[index + 1])
                    {
                        return None;
                    }
                    return Some(2);
                }
                return Some(1);
            }
            FlagValuePolicy::Optional => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if value.is_empty() {
                        return None;
                    }
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(value, token)
                    {
                        return None;
                    }
                    return Some(1);
                }
                if !remainder.is_empty() {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(remainder, token)
                    {
                        return None;
                    }
                    return Some(1);
                }
                if index + 1 < args.len() && !is_flag_like(args[index + 1].text.as_str()) {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_arg(&args[index + 1])
                    {
                        return None;
                    }
                    return Some(2);
                }
                return Some(1);
            }
        }
    }
    Some(1)
}

fn is_relative_path_arg(arg: &ParsedArg) -> bool {
    if arg.has_expansion {
        return false;
    }
    is_relative_path_text(arg.text.as_str())
}

fn is_relative_path_value(value: &str, source: &ParsedArg) -> bool {
    if source.has_expansion {
        return false;
    }
    is_relative_path_text(value)
}

fn is_relative_path_text(text: &str) -> bool {
    let normalized = normalize_path_text(text);
    if normalized.is_empty() {
        return false;
    }
    if normalized.starts_with('~') || normalized.starts_with('$') || normalized.starts_with("..") {
        return false;
    }
    !Path::new(normalized).is_absolute()
}

fn normalize_path_text(text: &str) -> &str {
    let trimmed = text.trim();
    if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        return &trimmed[1..trimmed.len() - 1];
    }
    trimmed
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
            let has_expansion = contains_expansion_nodes(arg_node);
            args.push(ParsedArg {
                text: arg,
                has_expansion,
            });
        }
    }

    Ok(ParsedCommand { name, args })
}

fn node_text(node: Node<'_>, source: &str) -> Option<String> {
    node.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

fn contains_expansion_nodes(node: Node<'_>) -> bool {
    if is_expansion_node_kind(node.kind()) {
        return true;
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if contains_expansion_nodes(child) {
            return true;
        }
    }

    false
}

fn is_expansion_node_kind(kind: &str) -> bool {
    matches!(
        kind,
        "expansion"
            | "simple_expansion"
            | "command_substitution"
            | "process_substitution"
            | "arithmetic_expansion"
            | "brace_expression"
    )
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
            if flags.is_empty() && !entry.allow_positional {
                return SafeCommandRule {
                    name: entry.name,
                    args: ArgPolicy::Deny,
                };
            }
            if entry.allow_any {
                return SafeCommandRule {
                    name: entry.name,
                    args: ArgPolicy::Any {
                        flags,
                        allow_positional: entry.allow_positional,
                        positional_path_from: entry.positional_path_from,
                    },
                };
            }
            SafeCommandRule {
                name: entry.name,
                args: ArgPolicy::AllowList {
                    flags,
                    allow_positional: entry.allow_positional,
                    positional_path_from: entry.positional_path_from,
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
        ls_flags.insert(
            "-l".to_string(),
            FlagPolicy {
                arg: FlagValuePolicy::None,
                value_type: FlagValueType::Any,
            },
        );
        ls_flags.insert(
            "-a".to_string(),
            FlagPolicy {
                arg: FlagValuePolicy::None,
                value_type: FlagValueType::Any,
            },
        );
        ls_flags.insert(
            "--color".to_string(),
            FlagPolicy {
                arg: FlagValuePolicy::Optional,
                value_type: FlagValueType::Any,
            },
        );
        let mut head_flags = HashMap::new();
        head_flags.insert(
            "-n".to_string(),
            FlagPolicy {
                arg: FlagValuePolicy::Required,
                value_type: FlagValueType::Any,
            },
        );
        let rules = vec![
            SafeCommandRule {
                name: "ls".to_string(),
                args: ArgPolicy::AllowList {
                    flags: ls_flags,
                    allow_positional: false,
                    positional_path_from: None,
                },
            },
            SafeCommandRule {
                name: "head".to_string(),
                args: ArgPolicy::AllowList {
                    flags: head_flags,
                    allow_positional: false,
                    positional_path_from: None,
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
    fn safe_command_rejects_non_relative_paths() {
        let mut grep_flags = HashMap::new();
        grep_flags.insert(
            "--file".to_string(),
            FlagPolicy {
                arg: FlagValuePolicy::Required,
                value_type: FlagValueType::Path,
            },
        );
        let rules = vec![
            SafeCommandRule {
                name: "cat".to_string(),
                args: ArgPolicy::AllowList {
                    flags: HashMap::new(),
                    allow_positional: true,
                    positional_path_from: Some(0),
                },
            },
            SafeCommandRule {
                name: "grep".to_string(),
                args: ArgPolicy::AllowList {
                    flags: grep_flags,
                    allow_positional: true,
                    positional_path_from: Some(1),
                },
            },
        ];

        assert_unsafe_cmd_with_rules!("cat /etc/passwd", &rules);
        assert_safe_cmd_with_rules!("cat ./etc/passwd", &rules);
        assert_unsafe_cmd_with_rules!("cat ~/etc/passwd", &rules);
        assert_unsafe_cmd_with_rules!("cat $HOME/etc/passwd", &rules);
        assert_safe_cmd_with_rules!("grep pattern ./file", &rules);
        assert_unsafe_cmd_with_rules!("grep pattern /etc/passwd", &rules);
        assert_safe_cmd_with_rules!("grep --file ./patterns file", &rules);
        assert_unsafe_cmd_with_rules!("grep --file /etc/passwd file", &rules);
    }

    #[test]
    fn safe_command_applies_path_checks_with_allow_any() {
        let mut flags = HashMap::new();
        flags.insert(
            "--file".to_string(),
            FlagPolicy {
                arg: FlagValuePolicy::Required,
                value_type: FlagValueType::Path,
            },
        );
        let rules = vec![SafeCommandRule {
            name: "cat".to_string(),
            args: ArgPolicy::Any {
                flags,
                allow_positional: true,
                positional_path_from: Some(0),
            },
        }];

        assert_unsafe_cmd_with_rules!("cat /etc/passwd", &rules);
        assert_safe_cmd_with_rules!("cat ./etc/passwd", &rules);
        assert_safe_cmd_with_rules!("cat --file ./etc/passwd", &rules);
        assert_unsafe_cmd_with_rules!("cat --file /etc/passwd", &rules);
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
