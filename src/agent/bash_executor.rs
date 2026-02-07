use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
    sync::{OnceLock, RwLock},
};

use lazy_static::lazy_static;
use tracing::warn;
use tree_sitter::{Node, Parser};

use crate::config::{
    ArgPolicy, BashConfig, BashConfigLayers, FlagPolicy, FlagValuePolicy, FlagValueType,
    SafeCommandRule, SafeCommandsMode, build_safe_command_rules_from_entries,
    load_safe_command_rules_from_path, parse_safe_command_rules,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArgMode {
    AllowList,
    AllowAny,
}

#[derive(Debug)]
struct ParsedArg {
    text: String,
    has_expansion: bool,
    byte_range: Range<usize>,
}

#[derive(Debug)]
struct ParsedCommand {
    name: String,
    args: Vec<ParsedArg>,
    name_range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommandSummary {
    pub name: String,
    pub args: Vec<String>,
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

const BUILTIN_SAFE_COMMANDS_TOML: &str = include_str!("safe_commands.toml");

lazy_static! {
    static ref BUILTIN_SAFE_COMMAND_RULES: Vec<SafeCommandRule> = load_builtin_safe_command_rules();
}

static SAFE_COMMAND_RULES: OnceLock<RwLock<Vec<SafeCommandRule>>> = OnceLock::new();

fn safe_command_rules() -> &'static RwLock<Vec<SafeCommandRule>> {
    SAFE_COMMAND_RULES.get_or_init(|| RwLock::new(BUILTIN_SAFE_COMMAND_RULES.clone()))
}

pub fn parse_primary_command(command: &str) -> Result<ParsedCommandSummary, String> {
    let commands = parse_commands(command).map_err(|err| err.to_reason().to_string())?;
    if commands.len() != 1 {
        return Err("multiple commands".to_string());
    }
    let command = commands
        .first()
        .ok_or_else(|| "command is empty".to_string())?;
    Ok(ParsedCommandSummary {
        name: command.name.clone(),
        args: command.args.iter().map(|arg| arg.text.clone()).collect(),
    })
}

pub fn bash_unsafe_ranges(command: &str) -> Vec<(Range<usize>, String)> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if command.contains('\n') || command.contains('\r') {
        return line_break_ranges(command)
            .into_iter()
            .map(|range| (range, "multi-line command".to_string()))
            .collect();
    }

    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .is_err()
    {
        return vec![(
            Range {
                start: 0,
                end: command.len(),
            },
            "bash parser unavailable".to_string(),
        )];
    }
    let Some(tree) = parser.parse(command, None) else {
        return vec![(
            Range {
                start: 0,
                end: command.len(),
            },
            "command parse failed".to_string(),
        )];
    };
    let root = tree.root_node();
    if root.has_error() || root.is_missing() {
        return vec![(
            Range {
                start: 0,
                end: command.len(),
            },
            "syntax error".to_string(),
        )];
    }
    let mut ranges: Vec<(Range<usize>, String)> = Vec::new();
    let mut token_ranges = Vec::new();
    let mut node_ranges = Vec::new();
    collect_disallowed_token_ranges(root, &mut token_ranges);
    collect_disallowed_node_ranges(root, &mut node_ranges);
    if !token_ranges.is_empty() || !node_ranges.is_empty() {
        ranges.extend(
            token_ranges
                .into_iter()
                .map(|range| (range, "unsupported shell operator".to_string())),
        );
        ranges.extend(
            node_ranges
                .into_iter()
                .map(|range| (range, "unsupported shell syntax".to_string())),
        );
        return ranges;
    }

    if root.named_child_count() != 1 {
        return vec![(
            Range {
                start: 0,
                end: command.len(),
            },
            "multiple statements".to_string(),
        )];
    }

    let mut commands = Vec::new();
    if collect_commands(root, command, &mut commands).is_err() {
        return vec![(
            Range {
                start: 0,
                end: command.len(),
            },
            "command parse failed".to_string(),
        )];
    }

    let rules = safe_command_rules().read().expect("safe command lock");
    for command in &commands {
        if let Some((rule, consumed_args)) = find_best_rule(command, &rules) {
            let remaining_args = command.args.get(consumed_args..).unwrap_or_default();
            ranges.extend(unsafe_args(remaining_args, &rule.args));
            continue;
        }

        if let Some(range) = find_chain_mismatch_range(command, &rules) {
            ranges.push((range, "command chain mismatch".to_string()));
            continue;
        }

        ranges.push((
            command.name_range.clone(),
            "command not allowlisted".to_string(),
        ));
    }

    ranges
}

impl ParseError {
    fn to_reason(&self) -> &'static str {
        match self {
            ParseError::Empty => "command is empty",
            ParseError::MultipleStatements => "multiple statements",
            ParseError::MissingCommandName => "missing command name",
            ParseError::ParseFailed => "command parse failed",
            ParseError::SyntaxError => "syntax error",
            ParseError::UnsupportedNode => "unsupported shell syntax",
        }
    }
}

pub fn bash_unsafe_reason(command: &str) -> Result<(), String> {
    let details = bash_unsafe_ranges(command);
    if details.is_empty() {
        return Ok(());
    }
    let mut seen = HashSet::new();
    let mut reasons = Vec::new();
    for (_, reason) in details {
        if seen.insert(reason.clone()) {
            reasons.push(reason);
        }
    }
    Err(reasons.join("; "))
}

pub(super) fn configure_safe_commands(
    layers: &BashConfigLayers,
    global_config_dir: &Path,
    workspace_config_dir: Option<&Path>,
    agent_config: Option<&crate::agent::config::AgentConfig>,
) {
    let mut rules = BUILTIN_SAFE_COMMAND_RULES.clone();
    if let Some(global) = layers.global.as_ref()
        && let Err(err) = apply_safe_command_layer(&mut rules, global, global_config_dir)
    {
        warn!("failed to apply global bash safe commands: {err}");
    }
    if let Some(workspace) = layers.workspace.as_ref() {
        let dir = workspace_config_dir.unwrap_or(global_config_dir);
        if let Err(err) = apply_safe_command_layer(&mut rules, workspace, dir) {
            warn!("failed to apply workspace bash safe commands: {err}");
        }
    }

    // Apply agent config safe commands (highest priority)
    if let Some(agent_cfg) = agent_config
        && let Some(safe_cmds) = agent_cfg.safe_commands.as_ref()
    {
        let dir = workspace_config_dir.unwrap_or(global_config_dir);
        if let Err(err) = apply_agent_safe_commands_layer(&mut rules, safe_cmds, dir) {
            warn!("failed to apply agent safe commands: {err}");
        }
    }

    let lock = safe_command_rules();
    let mut guard = lock.write().expect("safe command lock");
    *guard = rules;
}

#[cfg(test)]
fn is_safe_command(command: &str) -> bool {
    let rules = safe_command_rules().read().expect("safe command lock");
    is_safe_command_with_rules(command, &rules)
}

#[cfg(test)]
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
        let Some((rule, consumed_args)) = find_best_rule(command, rules) else {
            return false;
        };
        let remaining_args = command.args.get(consumed_args..).unwrap_or_default();
        is_safe_args(remaining_args, &rule.args)
    })
}

fn resolve_safe_commands_path(path: &str, config_dir: &Path) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_dir.join(path)
    }
}

fn apply_safe_command_layer(
    rules: &mut Vec<SafeCommandRule>,
    config: &BashConfig,
    config_dir: &Path,
) -> Result<(), String> {
    let Some(path) = config.safe_commands_path.as_deref() else {
        return Ok(());
    };
    let resolved = resolve_safe_commands_path(path, config_dir);
    let custom_rules = load_safe_command_rules_from_path(&resolved)
        .map_err(|err| format!("{}: {err}", resolved.display()))?;
    match config.safe_commands_mode {
        SafeCommandsMode::Append => rules.extend(custom_rules),
        SafeCommandsMode::Override => *rules = custom_rules,
    }
    Ok(())
}

fn apply_agent_safe_commands_layer(
    rules: &mut Vec<SafeCommandRule>,
    config: &crate::agent::config::SafeCommandsConfig,
    config_dir: &Path,
) -> Result<(), String> {
    use crate::agent::config::SafeCommandsConfig;

    let (mode, custom_rules) = match config {
        SafeCommandsConfig::Inline { mode, commands } => {
            let parsed_rules = build_safe_command_rules_from_entries(commands.clone());
            (*mode, parsed_rules)
        }
        SafeCommandsConfig::File { mode, path } => {
            let resolved =
                resolve_safe_commands_path(path.to_str().ok_or("invalid path")?, config_dir);
            let loaded_rules = load_safe_command_rules_from_path(&resolved)
                .map_err(|err| format!("{}: {err}", resolved.display()))?;
            (*mode, loaded_rules)
        }
    };

    match mode {
        crate::config::SafeCommandsMode::Append => rules.extend(custom_rules),
        crate::config::SafeCommandsMode::Override => *rules = custom_rules,
    }
    Ok(())
}

#[cfg(test)]
fn is_safe_args(args: &[ParsedArg], policy: &ArgPolicy) -> bool {
    match policy {
        ArgPolicy::Any {
            flags,
            allow_positional,
            positional_path_from,
            allow_dash,
        } => is_safe_args_with_mode(
            args,
            flags,
            *allow_positional,
            *positional_path_from,
            *allow_dash,
            ArgMode::AllowAny,
        ),
        ArgPolicy::Deny => args.is_empty(),
        ArgPolicy::AllowList {
            flags,
            allow_positional,
            positional_path_from,
            allow_dash,
        } => is_safe_args_with_mode(
            args,
            flags,
            *allow_positional,
            *positional_path_from,
            *allow_dash,
            ArgMode::AllowList,
        ),
    }
}

fn unsafe_args(args: &[ParsedArg], policy: &ArgPolicy) -> Vec<(Range<usize>, String)> {
    match policy {
        ArgPolicy::Any {
            flags,
            allow_positional,
            positional_path_from,
            allow_dash,
        } => unsafe_args_with_mode(
            args,
            flags,
            *allow_positional,
            *positional_path_from,
            *allow_dash,
            ArgMode::AllowAny,
        ),
        ArgPolicy::Deny => args
            .iter()
            .map(|arg| (arg.byte_range.clone(), "arguments not allowed".to_string()))
            .collect(),
        ArgPolicy::AllowList {
            flags,
            allow_positional,
            positional_path_from,
            allow_dash,
        } => unsafe_args_with_mode(
            args,
            flags,
            *allow_positional,
            *positional_path_from,
            *allow_dash,
            ArgMode::AllowList,
        ),
    }
}

fn unsafe_args_with_mode(
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    allow_positional: bool,
    positional_path_from: Option<usize>,
    allow_dash: bool,
    mode: ArgMode,
) -> Vec<(Range<usize>, String)> {
    let mut index = 0;
    let mut positional_index = 0;
    let mut options_ended = false;
    while index < args.len() {
        let arg = &args[index];
        let text = arg.text.as_str();

        if !options_ended && text == "--" {
            if !allow_positional {
                return vec![(
                    arg.byte_range.clone(),
                    "positional arguments not allowed".to_string(),
                )];
            }
            options_ended = true;
            index += 1;
            continue;
        }

        if !options_ended && is_long_option(text) {
            match check_long_option(arg, index, args, flags, mode) {
                OptionOutcome::Safe(consumed) => {
                    index += consumed;
                    continue;
                }
                OptionOutcome::Unsafe(range, reason) => return vec![(range, reason)],
            }
        }

        if !options_ended
            && is_short_option(text)
            && let Some(outcome) = check_single_dash_long_option(arg, index, args, flags, mode)
        {
            match outcome {
                OptionOutcome::Safe(consumed) => {
                    index += consumed;
                    continue;
                }
                OptionOutcome::Unsafe(range, reason) => return vec![(range, reason)],
            }
        }

        if !options_ended && is_short_option(text) {
            match check_short_options(arg, index, args, flags, mode) {
                OptionOutcome::Safe(consumed) => {
                    index += consumed;
                    continue;
                }
                OptionOutcome::Unsafe(range, reason) => return vec![(range, reason)],
            }
        }

        if text == "-" {
            if allow_positional {
                if positional_path_from
                    .map(|from| positional_index >= from)
                    .unwrap_or(false)
                    && !is_relative_path_arg(arg)
                {
                    return vec![(arg.byte_range.clone(), "path must be relative".to_string())];
                }
                positional_index += 1;
                index += 1;
                continue;
            }
            if allow_dash {
                index += 1;
                continue;
            }
        }

        if !allow_positional {
            return vec![(
                arg.byte_range.clone(),
                "positional arguments not allowed".to_string(),
            )];
        }
        if positional_path_from
            .map(|from| positional_index >= from)
            .unwrap_or(false)
            && !is_relative_path_arg(arg)
        {
            return vec![(arg.byte_range.clone(), "path must be relative".to_string())];
        }
        positional_index += 1;
        index += 1;
    }
    Vec::new()
}

#[cfg(test)]
fn is_safe_args_with_mode(
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    allow_positional: bool,
    positional_path_from: Option<usize>,
    allow_dash: bool,
    mode: ArgMode,
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
            let consumed = match consume_long_option(arg, index, args, flags, mode) {
                Some(consumed) => consumed,
                None => return false,
            };
            index += consumed;
            continue;
        }

        if !options_ended
            && is_short_option(text)
            && let Some(consumed) = consume_single_dash_long_option(arg, index, args, flags, mode)
        {
            index += consumed;
            continue;
        }

        if !options_ended && is_short_option(text) {
            let consumed = match parse_short_options(arg, index, args, flags, mode) {
                Some(consumed) => consumed,
                None => return false,
            };
            index += consumed;
            continue;
        }

        if text == "-" {
            if allow_positional {
                if positional_path_from
                    .map(|from| positional_index >= from)
                    .unwrap_or(false)
                    && !is_relative_path_arg(arg)
                {
                    return false;
                }
                positional_index += 1;
                index += 1;
                continue;
            }
            if allow_dash {
                index += 1;
                continue;
            }
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

fn split_single_dash_option(arg: &str) -> (&str, Option<&str>) {
    if let Some((name, value)) = arg.split_once('=') {
        (name, Some(value))
    } else {
        (arg, None)
    }
}

fn match_single_dash_long_option<'a>(
    arg: &'a str,
    flags: &HashMap<String, FlagPolicy>,
) -> Option<(&'a str, Option<&'a str>)> {
    if !arg.starts_with('-') || arg.starts_with("--") {
        return None;
    }
    let (name, attached_value) = split_single_dash_option(arg);
    if name.len() <= 2 {
        return None;
    }
    if flags.contains_key(name) {
        Some((name, attached_value))
    } else {
        None
    }
}

fn find_best_rule<'a>(
    command: &ParsedCommand,
    rules: &'a [SafeCommandRule],
) -> Option<(&'a SafeCommandRule, usize)> {
    let mut best: Option<(&SafeCommandRule, usize, usize)> = None;
    for rule in rules {
        let Some(consumed_args) = match_command_chain(command, rule) else {
            continue;
        };
        let chain_len = rule.command_chain.len();
        match best {
            Some((_, _, best_len)) if chain_len <= best_len => {}
            _ => best = Some((rule, consumed_args, chain_len)),
        }
    }
    best.map(|(rule, consumed_args, _)| (rule, consumed_args))
}

fn match_command_chain(command: &ParsedCommand, rule: &SafeCommandRule) -> Option<usize> {
    if rule.command_chain.is_empty() {
        return None;
    }
    if rule.command_chain[0] != command.name {
        return None;
    }
    let consumed_args = rule.command_chain.len() - 1;
    if command.args.len() < consumed_args {
        return None;
    }
    for (index, token) in rule.command_chain.iter().skip(1).enumerate() {
        if !command_chain_token_matches(command.args[index].text.as_str(), token.as_str()) {
            return None;
        }
    }
    Some(consumed_args)
}

fn find_chain_mismatch_range(
    command: &ParsedCommand,
    rules: &[SafeCommandRule],
) -> Option<Range<usize>> {
    let mut best: Option<(Range<usize>, usize)> = None;
    for rule in rules {
        if rule.command_chain.is_empty() || rule.command_chain[0] != command.name {
            continue;
        }
        let mut matched = 0;
        let mut mismatch = None;
        for (index, token) in rule.command_chain.iter().skip(1).enumerate() {
            let Some(arg) = command.args.get(index) else {
                mismatch = None;
                matched = index;
                break;
            };
            if !command_chain_token_matches(arg.text.as_str(), token.as_str()) {
                mismatch = Some(arg.byte_range.clone());
                matched = index;
                break;
            }
            matched = index + 1;
        }
        if let Some(range) = mismatch
            && best
                .as_ref()
                .is_none_or(|(_, best_len)| matched > *best_len)
        {
            best = Some((range, matched));
        }
    }
    best.map(|(range, _)| range)
}

fn command_chain_token_matches(arg: &str, token: &str) -> bool {
    token == ".*" || arg == token
}

#[cfg(test)]
fn consume_long_option(
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    mode: ArgMode,
) -> Option<usize> {
    let (name, attached_value) = split_long_option(token.text.as_str());
    consume_option_with_name(name, attached_value, token, index, args, flags, mode)
}

enum OptionOutcome {
    Safe(usize),
    Unsafe(Range<usize>, String),
}

fn check_long_option(
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    mode: ArgMode,
) -> OptionOutcome {
    let (name, attached_value) = split_long_option(token.text.as_str());
    check_option_with_name(name, attached_value, token, index, args, flags, mode)
}

#[cfg(test)]
fn consume_single_dash_long_option(
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    mode: ArgMode,
) -> Option<usize> {
    let (name, attached_value) = match_single_dash_long_option(token.text.as_str(), flags)?;
    consume_option_with_name(name, attached_value, token, index, args, flags, mode)
}

fn check_single_dash_long_option(
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    mode: ArgMode,
) -> Option<OptionOutcome> {
    let (name, attached_value) = match_single_dash_long_option(token.text.as_str(), flags)?;
    Some(check_option_with_name(
        name,
        attached_value,
        token,
        index,
        args,
        flags,
        mode,
    ))
}

#[cfg(test)]
fn consume_option_with_name(
    name: &str,
    attached_value: Option<&str>,
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    mode: ArgMode,
) -> Option<usize> {
    let policy = match flags.get(name) {
        Some(policy) => policy,
        None => {
            return if mode == ArgMode::AllowAny {
                Some(1)
            } else {
                None
            };
        }
    };
    match policy.arg {
        FlagValuePolicy::None => {
            if let Some(value) = attached_value {
                if mode == ArgMode::AllowAny {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(value, token)
                    {
                        return None;
                    }
                    return Some(1);
                }
                return None;
            }
            Some(1)
        }
        FlagValuePolicy::Required => {
            if let Some(value) = attached_value {
                if value.is_empty() {
                    return None;
                }
                if policy.value_type == FlagValueType::Path && !is_relative_path_value(value, token)
                {
                    return None;
                }
                return Some(1);
            }
            if index + 1 < args.len()
                && (mode == ArgMode::AllowList || !is_flag_like(args[index + 1].text.as_str()))
            {
                if policy.value_type == FlagValueType::Path
                    && !is_relative_path_arg(&args[index + 1])
                {
                    return None;
                }
                return Some(2);
            }
            if mode == ArgMode::AllowAny {
                Some(1)
            } else {
                None
            }
        }
        FlagValuePolicy::Optional => {
            if let Some(value) = attached_value {
                if value.is_empty() {
                    return None;
                }
                if policy.value_type == FlagValueType::Path && !is_relative_path_value(value, token)
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
            Some(1)
        }
    }
}

fn check_option_with_name(
    name: &str,
    attached_value: Option<&str>,
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    mode: ArgMode,
) -> OptionOutcome {
    let policy = match flags.get(name) {
        Some(policy) => policy,
        None => {
            return if mode == ArgMode::AllowAny {
                OptionOutcome::Safe(1)
            } else {
                OptionOutcome::Unsafe(token.byte_range.clone(), "flag not allowlisted".to_string())
            };
        }
    };
    match policy.arg {
        FlagValuePolicy::None => {
            if let Some(value) = attached_value {
                if mode == ArgMode::AllowAny {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(value, token)
                    {
                        return OptionOutcome::Unsafe(
                            token.byte_range.clone(),
                            "path must be relative".to_string(),
                        );
                    }
                    return OptionOutcome::Safe(1);
                }
                return OptionOutcome::Unsafe(
                    token.byte_range.clone(),
                    "flag does not accept value".to_string(),
                );
            }
            OptionOutcome::Safe(1)
        }
        FlagValuePolicy::Required => {
            if let Some(value) = attached_value {
                if value.is_empty() {
                    return OptionOutcome::Unsafe(
                        token.byte_range.clone(),
                        "flag value required".to_string(),
                    );
                }
                if policy.value_type == FlagValueType::Path && !is_relative_path_value(value, token)
                {
                    return OptionOutcome::Unsafe(
                        token.byte_range.clone(),
                        "path must be relative".to_string(),
                    );
                }
                return OptionOutcome::Safe(1);
            }
            if index + 1 < args.len()
                && (mode == ArgMode::AllowList || !is_flag_like(args[index + 1].text.as_str()))
            {
                if policy.value_type == FlagValueType::Path
                    && !is_relative_path_arg(&args[index + 1])
                {
                    return OptionOutcome::Unsafe(
                        args[index + 1].byte_range.clone(),
                        "path must be relative".to_string(),
                    );
                }
                return OptionOutcome::Safe(2);
            }
            if mode == ArgMode::AllowAny {
                OptionOutcome::Safe(1)
            } else {
                OptionOutcome::Unsafe(token.byte_range.clone(), "flag value required".to_string())
            }
        }
        FlagValuePolicy::Optional => {
            if let Some(value) = attached_value {
                if value.is_empty() {
                    return OptionOutcome::Unsafe(
                        token.byte_range.clone(),
                        "flag value required".to_string(),
                    );
                }
                if policy.value_type == FlagValueType::Path && !is_relative_path_value(value, token)
                {
                    return OptionOutcome::Unsafe(
                        token.byte_range.clone(),
                        "path must be relative".to_string(),
                    );
                }
                return OptionOutcome::Safe(1);
            }
            if index + 1 < args.len() && !is_flag_like(args[index + 1].text.as_str()) {
                if policy.value_type == FlagValueType::Path
                    && !is_relative_path_arg(&args[index + 1])
                {
                    return OptionOutcome::Unsafe(
                        args[index + 1].byte_range.clone(),
                        "path must be relative".to_string(),
                    );
                }
                return OptionOutcome::Safe(2);
            }
            OptionOutcome::Safe(1)
        }
    }
}

#[cfg(test)]
fn parse_short_options(
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    mode: ArgMode,
) -> Option<usize> {
    let body = token.text.strip_prefix('-')?;
    let mut offset = 0;
    while offset < body.len() {
        let mut iter = body[offset..].chars();
        let ch = iter.next()?;
        let ch_len = ch.len_utf8();
        let name = format!("-{ch}");
        let remainder = &body[offset + ch_len..];
        let policy = match flags.get(&name) {
            Some(policy) => policy,
            None => {
                if mode == ArgMode::AllowAny {
                    if remainder.starts_with('=') {
                        return Some(1);
                    }
                    offset += ch_len;
                    continue;
                }
                return None;
            }
        };
        match policy.arg {
            FlagValuePolicy::None => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if mode == ArgMode::AllowAny {
                        if policy.value_type == FlagValueType::Path
                            && !is_relative_path_value(value, token)
                        {
                            return None;
                        }
                        return Some(1);
                    }
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
                if index + 1 < args.len()
                    && (mode == ArgMode::AllowList || !is_flag_like(args[index + 1].text.as_str()))
                {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_arg(&args[index + 1])
                    {
                        return None;
                    }
                    return Some(2);
                }
                if mode == ArgMode::AllowAny {
                    return Some(1);
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

fn check_short_options(
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    mode: ArgMode,
) -> OptionOutcome {
    let Some(body) = token.text.strip_prefix('-') else {
        return OptionOutcome::Unsafe(token.byte_range.clone(), "flag parse failed".to_string());
    };
    let mut offset = 0;
    while offset < body.len() {
        let mut iter = body[offset..].chars();
        let Some(ch) = iter.next() else {
            return OptionOutcome::Unsafe(
                token.byte_range.clone(),
                "flag parse failed".to_string(),
            );
        };
        let ch_len = ch.len_utf8();
        let name = format!("-{ch}");
        let remainder = &body[offset + ch_len..];
        let policy = match flags.get(&name) {
            Some(policy) => policy,
            None => {
                if mode == ArgMode::AllowAny {
                    if remainder.starts_with('=') {
                        return OptionOutcome::Safe(1);
                    }
                    offset += ch_len;
                    continue;
                }
                return OptionOutcome::Unsafe(
                    token.byte_range.clone(),
                    "flag not allowlisted".to_string(),
                );
            }
        };
        match policy.arg {
            FlagValuePolicy::None => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if mode == ArgMode::AllowAny {
                        if policy.value_type == FlagValueType::Path
                            && !is_relative_path_value(value, token)
                        {
                            return OptionOutcome::Unsafe(
                                token.byte_range.clone(),
                                "path must be relative".to_string(),
                            );
                        }
                        return OptionOutcome::Safe(1);
                    }
                    return OptionOutcome::Unsafe(
                        token.byte_range.clone(),
                        "flag does not accept value".to_string(),
                    );
                }
                offset += ch_len;
            }
            FlagValuePolicy::Required => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if value.is_empty() {
                        return OptionOutcome::Unsafe(
                            token.byte_range.clone(),
                            "flag value required".to_string(),
                        );
                    }
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(value, token)
                    {
                        return OptionOutcome::Unsafe(
                            token.byte_range.clone(),
                            "path must be relative".to_string(),
                        );
                    }
                    return OptionOutcome::Safe(1);
                }
                if !remainder.is_empty() {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(remainder, token)
                    {
                        return OptionOutcome::Unsafe(
                            token.byte_range.clone(),
                            "path must be relative".to_string(),
                        );
                    }
                    return OptionOutcome::Safe(1);
                }
                if index + 1 < args.len()
                    && (mode == ArgMode::AllowList || !is_flag_like(args[index + 1].text.as_str()))
                {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_arg(&args[index + 1])
                    {
                        return OptionOutcome::Unsafe(
                            args[index + 1].byte_range.clone(),
                            "path must be relative".to_string(),
                        );
                    }
                    return OptionOutcome::Safe(2);
                }
                if mode == ArgMode::AllowAny {
                    return OptionOutcome::Safe(1);
                }
                return OptionOutcome::Unsafe(
                    token.byte_range.clone(),
                    "flag value required".to_string(),
                );
            }
            FlagValuePolicy::Optional => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if value.is_empty() {
                        return OptionOutcome::Unsafe(
                            token.byte_range.clone(),
                            "flag value required".to_string(),
                        );
                    }
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(value, token)
                    {
                        return OptionOutcome::Unsafe(
                            token.byte_range.clone(),
                            "path must be relative".to_string(),
                        );
                    }
                    return OptionOutcome::Safe(1);
                }
                if !remainder.is_empty() {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(remainder, token)
                    {
                        return OptionOutcome::Unsafe(
                            token.byte_range.clone(),
                            "path must be relative".to_string(),
                        );
                    }
                    return OptionOutcome::Safe(1);
                }
                if index + 1 < args.len() && !is_flag_like(args[index + 1].text.as_str()) {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_arg(&args[index + 1])
                    {
                        return OptionOutcome::Unsafe(
                            args[index + 1].byte_range.clone(),
                            "path must be relative".to_string(),
                        );
                    }
                    return OptionOutcome::Safe(2);
                }
                return OptionOutcome::Safe(1);
            }
        }
    }
    OptionOutcome::Safe(1)
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

    // Handle redirection nodes with safety checks
    if node.kind() == "redirected_statement" {
        // Check if this is a safe redirection
        validate_redirection(node, source)?;
        // Recursively process the redirected statement
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() != "file_redirect" {
                collect_commands(child, source, commands)?;
            }
        }
        return Ok(());
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
    let name_range = name_node.byte_range();

    let mut args = Vec::new();
    let mut cursor = node.walk();
    for arg_node in node.children_by_field_name("argument", &mut cursor) {
        if let Some(arg) = node_text(arg_node, source) {
            let has_expansion = contains_expansion_nodes(arg_node);
            args.push(ParsedArg {
                text: arg,
                has_expansion,
                byte_range: arg_node.byte_range(),
            });
        }
    }

    Ok(ParsedCommand {
        name,
        args,
        name_range,
    })
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

/// Validates that a redirection is safe.
///
/// Allows:
/// - Redirection to /dev/null (e.g., > /dev/null, 2> /dev/null)
/// - File descriptor duplication (e.g., 2>&1, 1>&2)
///
/// Disallows:
/// - Redirection to arbitrary files
fn validate_redirection(node: Node<'_>, source: &str) -> Result<(), ParseError> {
    let mut cursor = node.walk();

    // Check all file_redirect children
    for child in node.named_children(&mut cursor) {
        if child.kind() == "file_redirect" && !is_safe_file_redirect(&child, source) {
            return Err(ParseError::UnsupportedNode);
        }
    }

    Ok(())
}

/// Checks if a file_redirect node is safe
fn is_safe_file_redirect(node: &Node<'_>, source: &str) -> bool {
    // Check if this is a fd duplication (has >& or <& operator)
    let is_fd_dup = has_fd_dup_operator(node);

    if is_fd_dup {
        // For fd duplication, destination is a number (e.g., "1" in 2>&1)
        if let Some(dest_node) = node.child_by_field_name("destination")
            && dest_node.kind() == "number"
            && let Some(text) = node_text(dest_node, source)
            && let Ok(num) = text.parse::<u32>()
        {
            return num <= 9;
        }
        return false;
    }

    // For regular redirection, check destination path
    if let Some(dest_node) = node.child_by_field_name("destination")
        && let Some(dest) = node_text(dest_node, source)
    {
        return dest.trim() == "/dev/null";
    }

    false
}

/// Checks if a file_redirect contains a fd duplication operator (>& or <&)
fn has_fd_dup_operator(node: &Node<'_>) -> bool {
    let mut index = 0;
    let count = node.child_count();
    while index < count {
        if let Some(child) = node.child(index) {
            let kind = child.kind();
            if kind == ">&" || kind == "<&" {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn collect_disallowed_token_ranges(node: Node<'_>, ranges: &mut Vec<Range<usize>>) {
    if is_disallowed_token_kind(node.kind()) {
        ranges.push(node.byte_range());
    }
    let mut index = 0;
    let count = node.child_count();
    while index < count {
        if let Some(child) = node.child(index) {
            collect_disallowed_token_ranges(child, ranges);
        }
        index += 1;
    }
}

fn collect_disallowed_node_ranges(node: Node<'_>, ranges: &mut Vec<Range<usize>>) {
    if is_disallowed_node_kind(node.kind()) {
        ranges.push(node.byte_range());
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_disallowed_node_ranges(child, ranges);
    }
}

fn line_break_ranges(command: &str) -> Vec<Range<usize>> {
    command
        .char_indices()
        .filter_map(|(idx, ch)| {
            if ch == '\n' || ch == '\r' {
                Some(idx..idx + ch.len_utf8())
            } else {
                None
            }
        })
        .collect()
}

fn load_builtin_safe_command_rules() -> Vec<SafeCommandRule> {
    match parse_safe_command_rules(BUILTIN_SAFE_COMMANDS_TOML) {
        Ok(rules) => rules,
        Err(err) => {
            warn!("failed to parse builtin safe commands: {err}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn write_safe_commands_file(dir: &Path, name: &str, command: &str) -> PathBuf {
        let path = dir.join(name);
        let content = format!("[[commands]]\nname = \"{command}\"\nallow_any = true\n");
        std::fs::write(&path, content).expect("write safe commands file");
        path
    }

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
                command_chain: vec!["ls".to_string()],
                args: ArgPolicy::AllowList {
                    flags: ls_flags,
                    allow_positional: false,
                    positional_path_from: None,
                    allow_dash: false,
                },
            },
            SafeCommandRule {
                command_chain: vec!["head".to_string()],
                args: ArgPolicy::AllowList {
                    flags: head_flags,
                    allow_positional: false,
                    positional_path_from: None,
                    allow_dash: false,
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
                command_chain: vec!["cat".to_string()],
                args: ArgPolicy::AllowList {
                    flags: HashMap::new(),
                    allow_positional: true,
                    positional_path_from: Some(0),
                    allow_dash: false,
                },
            },
            SafeCommandRule {
                command_chain: vec!["grep".to_string()],
                args: ArgPolicy::AllowList {
                    flags: grep_flags,
                    allow_positional: true,
                    positional_path_from: Some(1),
                    allow_dash: false,
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
            command_chain: vec!["cat".to_string()],
            args: ArgPolicy::Any {
                flags,
                allow_positional: true,
                positional_path_from: Some(0),
                allow_dash: false,
            },
        }];

        assert_unsafe_cmd_with_rules!("cat /etc/passwd", &rules);
        assert_safe_cmd_with_rules!("cat ./etc/passwd", &rules);
        assert_safe_cmd_with_rules!("cat --file ./etc/passwd", &rules);
        assert_unsafe_cmd_with_rules!("cat --file /etc/passwd", &rules);
    }

    #[test]
    fn safe_command_allows_single_dash_argument_when_configured() {
        let rules = vec![SafeCommandRule {
            command_chain: vec!["cat".to_string()],
            args: ArgPolicy::AllowList {
                flags: HashMap::new(),
                allow_positional: false,
                positional_path_from: None,
                allow_dash: true,
            },
        }];

        assert_safe_cmd_with_rules!("cat -", &rules);
        assert_unsafe_cmd_with_rules!("cat ./file", &rules);
    }

    #[test]
    fn safe_command_allows_single_dash_long_option_with_value() {
        let mut flags = HashMap::new();
        flags.insert(
            "-name".to_string(),
            FlagPolicy {
                arg: FlagValuePolicy::Required,
                value_type: FlagValueType::Any,
            },
        );
        let rules = vec![SafeCommandRule {
            command_chain: vec!["find".to_string()],
            args: ArgPolicy::AllowList {
                flags,
                allow_positional: false,
                positional_path_from: None,
                allow_dash: false,
            },
        }];

        assert_safe_cmd_with_rules!("find -name main.rs", &rules);
        assert_safe_cmd_with_rules!("find -name=main.rs", &rules);
        assert_unsafe_cmd_with_rules!("find -name", &rules);
        assert_unsafe_cmd_with_rules!("find -na main.rs", &rules);
    }

    #[test]
    fn safe_command_matches_command_chain() {
        let rules = vec![
            SafeCommandRule {
                command_chain: vec!["git".to_string()],
                args: ArgPolicy::Any {
                    flags: HashMap::new(),
                    allow_positional: true,
                    positional_path_from: None,
                    allow_dash: false,
                },
            },
            SafeCommandRule {
                command_chain: vec!["git".to_string(), "status".to_string()],
                args: ArgPolicy::AllowList {
                    flags: HashMap::new(),
                    allow_positional: true,
                    positional_path_from: None,
                    allow_dash: false,
                },
            },
            SafeCommandRule {
                command_chain: vec!["cargo".to_string(), "check".to_string()],
                args: ArgPolicy::AllowList {
                    flags: HashMap::new(),
                    allow_positional: true,
                    positional_path_from: None,
                    allow_dash: false,
                },
            },
        ];

        assert_safe_cmd_with_rules!("git status", &rules);
        assert_unsafe_cmd_with_rules!("git status -s", &rules);
        assert_safe_cmd_with_rules!("git checkout", &rules);
        assert_safe_cmd_with_rules!("cargo check", &rules);
        assert_unsafe_cmd_with_rules!("cargo build", &rules);
    }

    #[test]
    fn safe_command_matches_wildcard_chain_token() {
        let rules = vec![SafeCommandRule {
            command_chain: vec![
                "coco".to_string(),
                "mcp".to_string(),
                ".*".to_string(),
                "--help".to_string(),
            ],
            args: ArgPolicy::AllowList {
                flags: HashMap::new(),
                allow_positional: false,
                positional_path_from: None,
                allow_dash: false,
            },
        }];

        assert_safe_cmd_with_rules!("coco mcp list --help", &rules);
        assert_safe_cmd_with_rules!("coco mcp server --help", &rules);
        assert_unsafe_cmd_with_rules!("coco mcp --help", &rules);
        assert_unsafe_cmd_with_rules!("coco mcp list --help --verbose", &rules);
    }

    #[test]
    fn unsafe_command_rejects_shell_chaining_or_substitution() {
        assert_unsafe_cmd!("ls; rm -rf /");
        assert_unsafe_cmd!("ls &");
        assert_unsafe_cmd!("echo $(whoami)");
        assert_unsafe_cmd!("ls > out.txt");
    }

    #[test]
    fn safe_command_allows_dev_null_redirection() {
        assert_safe_cmd!("ls > /dev/null");
        assert_safe_cmd!("ls 2>/dev/null");
        assert_safe_cmd!("ls 1>/dev/null");
        assert_safe_cmd!("cat file.txt > /dev/null 2>&1");
    }

    #[test]
    fn safe_command_allows_fd_duplication() {
        assert_safe_cmd!("ls 2>&1");
        assert_safe_cmd!("ls 1>&2");
        assert_safe_cmd!("cat file.txt 2>&1");
    }

    #[test]
    fn safe_command_allows_combined_redirections() {
        assert_safe_cmd!("ls > /dev/null 2>&1");
        assert_safe_cmd!("cat file.txt 2>&1 | head");
        assert_safe_cmd!("ls 2>/dev/null | grep pattern");
    }

    #[test]
    fn unsafe_command_rejects_file_redirection() {
        assert_unsafe_cmd!("ls > out.txt");
        assert_unsafe_cmd!("ls >> out.txt");
        assert_unsafe_cmd!("ls 2> error.log");
        assert_unsafe_cmd!("ls > /tmp/out.txt");
    }

    #[test]
    fn unsafe_command_rejects_background_execution() {
        assert_unsafe_cmd!("ls &");
        assert_unsafe_cmd!("sleep 10 &");
        assert_unsafe_cmd!("cat file.txt &");
    }

    #[test]
    fn unsafe_command_rejects_unknown_or_empty() {
        assert_unsafe_cmd!("");
        assert_unsafe_cmd!("   ");
        assert_unsafe_cmd!("bash -c ls");
        assert_unsafe_cmd!("sudo ls");
    }

    #[test]
    fn safe_command_layers_apply_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _global_path = write_safe_commands_file(dir.path(), "global.toml", "global_cmd");
        let _workspace_path =
            write_safe_commands_file(dir.path(), "workspace.toml", "workspace_cmd");

        let base_rules =
            parse_safe_command_rules("[[commands]]\nname = \"base_cmd\"\nallow_any = true\n")
                .expect("parse base rules");
        let mut rules = base_rules;

        let global = BashConfig {
            safe_commands_path: Some("global.toml".to_string()),
            safe_commands_mode: SafeCommandsMode::Append,
        };
        apply_safe_command_layer(&mut rules, &global, dir.path()).expect("apply global");
        assert_safe_cmd_with_rules!("base_cmd", &rules);
        assert_safe_cmd_with_rules!("global_cmd", &rules);

        let workspace = BashConfig {
            safe_commands_path: Some("workspace.toml".to_string()),
            safe_commands_mode: SafeCommandsMode::Override,
        };
        apply_safe_command_layer(&mut rules, &workspace, dir.path()).expect("apply workspace");
        assert_unsafe_cmd_with_rules!("base_cmd", &rules);
        assert_unsafe_cmd_with_rules!("global_cmd", &rules);
        assert_safe_cmd_with_rules!("workspace_cmd", &rules);
    }

    #[test]
    fn safe_command_layers_override_then_append() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _global_path = write_safe_commands_file(dir.path(), "global.toml", "global_cmd");
        let _workspace_path =
            write_safe_commands_file(dir.path(), "workspace.toml", "workspace_cmd");

        let base_rules =
            parse_safe_command_rules("[[commands]]\nname = \"base_cmd\"\nallow_any = true\n")
                .expect("parse base rules");
        let mut rules = base_rules;

        let global = BashConfig {
            safe_commands_path: Some("global.toml".to_string()),
            safe_commands_mode: SafeCommandsMode::Override,
        };
        apply_safe_command_layer(&mut rules, &global, dir.path()).expect("apply global");
        assert_unsafe_cmd_with_rules!("base_cmd", &rules);
        assert_safe_cmd_with_rules!("global_cmd", &rules);

        let workspace = BashConfig {
            safe_commands_path: Some("workspace.toml".to_string()),
            safe_commands_mode: SafeCommandsMode::Append,
        };
        apply_safe_command_layer(&mut rules, &workspace, dir.path()).expect("apply workspace");
        assert_safe_cmd_with_rules!("global_cmd", &rules);
        assert_safe_cmd_with_rules!("workspace_cmd", &rules);
    }

    fn assert_unsafe_range_contains(command: &str, needle: &str, reason: &str) {
        let ranges = bash_unsafe_ranges(command);
        assert!(
            ranges
                .iter()
                .any(|(range, detail)| command[range.clone()].contains(needle)
                    && detail.contains(reason)),
            "expected unsafe range containing {needle:?} with reason {reason:?}, got {ranges:?}"
        );
    }

    #[test]
    fn bash_unsafe_ranges_empty_for_safe_command() {
        let command = "ls -la";
        let ranges = bash_unsafe_ranges(command);
        assert!(
            ranges.is_empty(),
            "expected no unsafe ranges for {command:?}, got {ranges:?}"
        );
    }

    #[test]
    fn bash_unsafe_ranges_marks_absolute_path() {
        let command = "cat /etc/passwd";
        assert_unsafe_range_contains(command, "/etc/passwd", "path must be relative");
    }

    #[test]
    fn bash_unsafe_ranges_marks_disallowed_token() {
        let command = "ls; rm -rf /";
        assert_unsafe_range_contains(command, ";", "unsupported shell operator");
    }

    #[test]
    fn bash_unsafe_ranges_marks_unknown_command() {
        let command = "rm -rf /";
        assert_unsafe_range_contains(command, "rm", "command not allowlisted");
    }

    #[test]
    fn bash_unsafe_reason_reports_reason() {
        let command = "rm -rf /";
        let reason = bash_unsafe_reason(command).expect_err("expected unsafe reason");
        assert!(
            reason.contains("command not allowlisted"),
            "unexpected reason: {reason:?}"
        );
    }

    #[test]
    fn agent_config_inline_safe_commands_override() {
        use crate::agent::config::SafeCommandsConfig;
        use crate::config::{SafeCommandEntry, SafeCommandsMode};

        let mut rules = BUILTIN_SAFE_COMMAND_RULES.clone();

        // Create agent config with inline safe commands that override builtin
        let agent_config = crate::agent::config::AgentConfig {
            safe_commands: Some(SafeCommandsConfig::Inline {
                mode: SafeCommandsMode::Override,
                commands: vec![SafeCommandEntry {
                    name: Some("custom_cmd".to_string()),
                    allow_any: true,
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };

        let dir = tempfile::tempdir().expect("tempdir");
        apply_agent_safe_commands_layer(
            &mut rules,
            agent_config.safe_commands.as_ref().unwrap(),
            dir.path(),
        )
        .expect("apply agent safe commands");

        // Only custom_cmd should be safe (override mode)
        assert_safe_cmd_with_rules!("custom_cmd", &rules);
        assert_unsafe_cmd_with_rules!("ls", &rules); // builtin command should be gone
    }

    #[test]
    fn agent_config_inline_safe_commands_append() {
        use crate::agent::config::SafeCommandsConfig;
        use crate::config::{SafeCommandEntry, SafeCommandsMode};

        let mut rules = BUILTIN_SAFE_COMMAND_RULES.clone();

        // Create agent config with inline safe commands that append to builtin
        let agent_config = crate::agent::config::AgentConfig {
            safe_commands: Some(SafeCommandsConfig::Inline {
                mode: SafeCommandsMode::Append,
                commands: vec![SafeCommandEntry {
                    name: Some("custom_cmd".to_string()),
                    allow_any: true,
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };

        let dir = tempfile::tempdir().expect("tempdir");
        apply_agent_safe_commands_layer(
            &mut rules,
            agent_config.safe_commands.as_ref().unwrap(),
            dir.path(),
        )
        .expect("apply agent safe commands");

        // Both custom_cmd and builtin commands should be safe
        assert_safe_cmd_with_rules!("custom_cmd", &rules);
        assert_safe_cmd_with_rules!("ls", &rules); // builtin command should still exist
    }

    #[test]
    fn agent_config_file_safe_commands() {
        use crate::agent::config::SafeCommandsConfig;
        use crate::config::SafeCommandsMode;
        use std::path::PathBuf;

        let dir = tempfile::tempdir().expect("tempdir");
        let _safe_file_path = write_safe_commands_file(dir.path(), "agent_safe.toml", "agent_cmd");

        let mut rules = BUILTIN_SAFE_COMMAND_RULES.clone();

        // Create agent config with file-based safe commands
        let agent_config = crate::agent::config::AgentConfig {
            safe_commands: Some(SafeCommandsConfig::File {
                mode: SafeCommandsMode::Append,
                path: PathBuf::from("agent_safe.toml"),
            }),
            ..Default::default()
        };

        apply_agent_safe_commands_layer(
            &mut rules,
            agent_config.safe_commands.as_ref().unwrap(),
            dir.path(),
        )
        .expect("apply agent safe commands");

        // agent_cmd from file should be added
        assert_safe_cmd_with_rules!("agent_cmd", &rules);
        assert_safe_cmd_with_rules!("ls", &rules); // builtin should still exist
    }

    #[test]
    fn git_fetch_is_safe_command() {
        // Test that git fetch is recognized as a safe command
        assert_safe_cmd!("git fetch");
        assert_safe_cmd!("git fetch origin");
        assert_safe_cmd!("git fetch origin main");
        assert_safe_cmd!("git fetch --all");
        assert_safe_cmd!("git fetch --prune");
        assert_safe_cmd!("git fetch --multiple origin upstream");
    }
}
