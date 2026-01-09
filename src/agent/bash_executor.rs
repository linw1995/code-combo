use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    sync::{OnceLock, RwLock},
};

use lazy_static::lazy_static;
use serde::Deserialize;
use toml as serde_toml;
use tracing::warn;
use tree_sitter::{Node, Parser};

use crate::{
    config::{BashConfig, BashConfigLayers, SafeCommandsMode},
    tools::BashInput,
};

#[derive(Clone)]
struct SafeCommandRule {
    command_chain: Vec<String>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArgMode {
    AllowList,
    AllowAny,
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
    byte_range: Range<usize>,
}

#[derive(Debug)]
struct ParsedCommand {
    name: String,
    args: Vec<ParsedArg>,
    name_range: Range<usize>,
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

#[derive(Deserialize)]
struct SafeCommandConfig {
    commands: Vec<SafeCommandConfigEntry>,
}

#[derive(Deserialize)]
struct SafeCommandConfigEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    command: Option<Vec<String>>,
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
    static ref BUILTIN_SAFE_COMMAND_RULES: Vec<SafeCommandRule> = load_builtin_safe_command_rules();
}

static SAFE_COMMAND_RULES: OnceLock<RwLock<Vec<SafeCommandRule>>> = OnceLock::new();

fn safe_command_rules() -> &'static RwLock<Vec<SafeCommandRule>> {
    SAFE_COMMAND_RULES.get_or_init(|| RwLock::new(BUILTIN_SAFE_COMMAND_RULES.clone()))
}

pub fn should_bypass_permission(input: &BashInput) -> bool {
    is_safe_command(&input.command)
}

pub fn bash_unsafe_ranges(command: &str) -> Vec<Range<usize>> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if command.contains('\n') || command.contains('\r') {
        return normalize_ranges(line_break_ranges(command), command.len());
    }

    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .is_err()
    {
        return full_command_range(command.len());
    }
    let Some(tree) = parser.parse(command, None) else {
        return full_command_range(command.len());
    };
    let root = tree.root_node();
    if root.has_error() || root.is_missing() {
        return full_command_range(command.len());
    }
    if root.named_child_count() != 1 {
        return full_command_range(command.len());
    }

    let mut ranges = Vec::new();
    collect_disallowed_token_ranges(root, &mut ranges);
    collect_disallowed_node_ranges(root, &mut ranges);
    if !ranges.is_empty() {
        return normalize_ranges(ranges, command.len());
    }

    let mut commands = Vec::new();
    if collect_commands(root, command, &mut commands).is_err() {
        return full_command_range(command.len());
    }

    let rules = safe_command_rules().read().expect("safe command lock");
    for command in &commands {
        if let Some((rule, consumed_args)) = find_best_rule(command, &rules) {
            let remaining_args = command.args.get(consumed_args..).unwrap_or_default();
            ranges.extend(unsafe_args(remaining_args, &rule.args));
            continue;
        }

        if let Some(range) = find_chain_mismatch_range(command, &rules) {
            ranges.push(range);
            continue;
        }

        ranges.push(command.name_range.clone());
    }

    normalize_ranges(ranges, command.len())
}

fn full_command_range(len: usize) -> Vec<Range<usize>> {
    vec![Range { start: 0, end: len }]
}

pub(super) fn configure_safe_commands(
    layers: &BashConfigLayers,
    global_config_dir: &Path,
    workspace_config_dir: Option<&Path>,
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

    let lock = safe_command_rules();
    let mut guard = lock.write().expect("safe command lock");
    *guard = rules;
}

fn is_safe_command(command: &str) -> bool {
    let rules = safe_command_rules().read().expect("safe command lock");
    is_safe_command_with_rules(command, &rules)
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

fn is_safe_args(args: &[ParsedArg], policy: &ArgPolicy) -> bool {
    match policy {
        ArgPolicy::Any {
            flags,
            allow_positional,
            positional_path_from,
        } => is_safe_args_with_mode(
            args,
            flags,
            *allow_positional,
            *positional_path_from,
            ArgMode::AllowAny,
        ),
        ArgPolicy::Deny => args.is_empty(),
        ArgPolicy::AllowList {
            flags,
            allow_positional,
            positional_path_from,
        } => is_safe_args_with_mode(
            args,
            flags,
            *allow_positional,
            *positional_path_from,
            ArgMode::AllowList,
        ),
    }
}

fn unsafe_args(args: &[ParsedArg], policy: &ArgPolicy) -> Vec<Range<usize>> {
    match policy {
        ArgPolicy::Any {
            flags,
            allow_positional,
            positional_path_from,
        } => unsafe_args_with_mode(
            args,
            flags,
            *allow_positional,
            *positional_path_from,
            ArgMode::AllowAny,
        ),
        ArgPolicy::Deny => args.iter().map(|arg| arg.byte_range.clone()).collect(),
        ArgPolicy::AllowList {
            flags,
            allow_positional,
            positional_path_from,
        } => unsafe_args_with_mode(
            args,
            flags,
            *allow_positional,
            *positional_path_from,
            ArgMode::AllowList,
        ),
    }
}

fn unsafe_args_with_mode(
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    allow_positional: bool,
    positional_path_from: Option<usize>,
    mode: ArgMode,
) -> Vec<Range<usize>> {
    let mut index = 0;
    let mut positional_index = 0;
    let mut options_ended = false;
    while index < args.len() {
        let arg = &args[index];
        let text = arg.text.as_str();

        if !options_ended && text == "--" {
            if !allow_positional {
                return vec![arg.byte_range.clone()];
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
                OptionOutcome::Unsafe(range) => return vec![range],
            }
        }

        if !options_ended && is_short_option(text) {
            match check_short_options(arg, index, args, flags, mode) {
                OptionOutcome::Safe(consumed) => {
                    index += consumed;
                    continue;
                }
                OptionOutcome::Unsafe(range) => return vec![range],
            }
        }

        if !allow_positional {
            return vec![arg.byte_range.clone()];
        }
        if positional_path_from
            .map(|from| positional_index >= from)
            .unwrap_or(false)
            && !is_relative_path_arg(arg)
        {
            return vec![arg.byte_range.clone()];
        }
        positional_index += 1;
        index += 1;
    }
    Vec::new()
}

fn is_safe_args_with_mode(
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    allow_positional: bool,
    positional_path_from: Option<usize>,
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

        if !options_ended && is_short_option(text) {
            let consumed = match parse_short_options(arg, index, args, flags, mode) {
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

fn consume_long_option(
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    mode: ArgMode,
) -> Option<usize> {
    let (name, attached_value) = split_long_option(token.text.as_str());
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

enum OptionOutcome {
    Safe(usize),
    Unsafe(Range<usize>),
}

fn check_long_option(
    token: &ParsedArg,
    index: usize,
    args: &[ParsedArg],
    flags: &HashMap<String, FlagPolicy>,
    mode: ArgMode,
) -> OptionOutcome {
    let (name, attached_value) = split_long_option(token.text.as_str());
    let policy = match flags.get(name) {
        Some(policy) => policy,
        None => {
            return if mode == ArgMode::AllowAny {
                OptionOutcome::Safe(1)
            } else {
                OptionOutcome::Unsafe(token.byte_range.clone())
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
                        return OptionOutcome::Unsafe(token.byte_range.clone());
                    }
                    return OptionOutcome::Safe(1);
                }
                return OptionOutcome::Unsafe(token.byte_range.clone());
            }
            OptionOutcome::Safe(1)
        }
        FlagValuePolicy::Required => {
            if let Some(value) = attached_value {
                if value.is_empty() {
                    return OptionOutcome::Unsafe(token.byte_range.clone());
                }
                if policy.value_type == FlagValueType::Path && !is_relative_path_value(value, token)
                {
                    return OptionOutcome::Unsafe(token.byte_range.clone());
                }
                return OptionOutcome::Safe(1);
            }
            if index + 1 < args.len()
                && (mode == ArgMode::AllowList || !is_flag_like(args[index + 1].text.as_str()))
            {
                if policy.value_type == FlagValueType::Path
                    && !is_relative_path_arg(&args[index + 1])
                {
                    return OptionOutcome::Unsafe(args[index + 1].byte_range.clone());
                }
                return OptionOutcome::Safe(2);
            }
            if mode == ArgMode::AllowAny {
                OptionOutcome::Safe(1)
            } else {
                OptionOutcome::Unsafe(token.byte_range.clone())
            }
        }
        FlagValuePolicy::Optional => {
            if let Some(value) = attached_value {
                if value.is_empty() {
                    return OptionOutcome::Unsafe(token.byte_range.clone());
                }
                if policy.value_type == FlagValueType::Path && !is_relative_path_value(value, token)
                {
                    return OptionOutcome::Unsafe(token.byte_range.clone());
                }
                return OptionOutcome::Safe(1);
            }
            if index + 1 < args.len() && !is_flag_like(args[index + 1].text.as_str()) {
                if policy.value_type == FlagValueType::Path
                    && !is_relative_path_arg(&args[index + 1])
                {
                    return OptionOutcome::Unsafe(args[index + 1].byte_range.clone());
                }
                return OptionOutcome::Safe(2);
            }
            OptionOutcome::Safe(1)
        }
    }
}

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
        return OptionOutcome::Unsafe(token.byte_range.clone());
    };
    let mut offset = 0;
    while offset < body.len() {
        let mut iter = body[offset..].chars();
        let Some(ch) = iter.next() else {
            return OptionOutcome::Unsafe(token.byte_range.clone());
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
                return OptionOutcome::Unsafe(token.byte_range.clone());
            }
        };
        match policy.arg {
            FlagValuePolicy::None => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if mode == ArgMode::AllowAny {
                        if policy.value_type == FlagValueType::Path
                            && !is_relative_path_value(value, token)
                        {
                            return OptionOutcome::Unsafe(token.byte_range.clone());
                        }
                        return OptionOutcome::Safe(1);
                    }
                    return OptionOutcome::Unsafe(token.byte_range.clone());
                }
                offset += ch_len;
            }
            FlagValuePolicy::Required => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if value.is_empty() {
                        return OptionOutcome::Unsafe(token.byte_range.clone());
                    }
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(value, token)
                    {
                        return OptionOutcome::Unsafe(token.byte_range.clone());
                    }
                    return OptionOutcome::Safe(1);
                }
                if !remainder.is_empty() {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(remainder, token)
                    {
                        return OptionOutcome::Unsafe(token.byte_range.clone());
                    }
                    return OptionOutcome::Safe(1);
                }
                if index + 1 < args.len()
                    && (mode == ArgMode::AllowList || !is_flag_like(args[index + 1].text.as_str()))
                {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_arg(&args[index + 1])
                    {
                        return OptionOutcome::Unsafe(args[index + 1].byte_range.clone());
                    }
                    return OptionOutcome::Safe(2);
                }
                if mode == ArgMode::AllowAny {
                    return OptionOutcome::Safe(1);
                }
                return OptionOutcome::Unsafe(token.byte_range.clone());
            }
            FlagValuePolicy::Optional => {
                if let Some(value) = remainder.strip_prefix('=') {
                    if value.is_empty() {
                        return OptionOutcome::Unsafe(token.byte_range.clone());
                    }
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(value, token)
                    {
                        return OptionOutcome::Unsafe(token.byte_range.clone());
                    }
                    return OptionOutcome::Safe(1);
                }
                if !remainder.is_empty() {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_value(remainder, token)
                    {
                        return OptionOutcome::Unsafe(token.byte_range.clone());
                    }
                    return OptionOutcome::Safe(1);
                }
                if index + 1 < args.len() && !is_flag_like(args[index + 1].text.as_str()) {
                    if policy.value_type == FlagValueType::Path
                        && !is_relative_path_arg(&args[index + 1])
                    {
                        return OptionOutcome::Unsafe(args[index + 1].byte_range.clone());
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

fn normalize_ranges(ranges: Vec<Range<usize>>, source_len: usize) -> Vec<Range<usize>> {
    let mut normalized = ranges
        .into_iter()
        .filter_map(|range| {
            let start = range.start.min(source_len);
            let end = range.end.min(source_len);
            if start < end { Some(start..end) } else { None }
        })
        .collect::<Vec<_>>();
    normalized.sort_by_key(|range| range.start);

    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in normalized {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
            continue;
        }
        merged.push(range);
    }
    merged
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

fn load_safe_command_rules_from_path(path: &Path) -> Result<Vec<SafeCommandRule>, String> {
    let content =
        std::fs::read_to_string(path).map_err(|err| format!("failed to read file: {err}"))?;
    parse_safe_command_rules(&content)
}

fn parse_safe_command_rules(source: &str) -> Result<Vec<SafeCommandRule>, String> {
    let config: SafeCommandConfig =
        serde_toml::from_str(source).map_err(|err| format!("failed to parse config: {err}"))?;
    Ok(build_safe_command_rules(config))
}

fn build_safe_command_rules(config: SafeCommandConfig) -> Vec<SafeCommandRule> {
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
            if flags.is_empty() && !allow_positional {
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
                    },
                };
            }
            SafeCommandRule {
                command_chain,
                args: ArgPolicy::AllowList {
                    flags,
                    allow_positional,
                    positional_path_from: entry.positional_path_from,
                },
            }
        })
        .collect()
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
                },
            },
            SafeCommandRule {
                command_chain: vec!["head".to_string()],
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
                command_chain: vec!["cat".to_string()],
                args: ArgPolicy::AllowList {
                    flags: HashMap::new(),
                    allow_positional: true,
                    positional_path_from: Some(0),
                },
            },
            SafeCommandRule {
                command_chain: vec!["grep".to_string()],
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
            command_chain: vec!["cat".to_string()],
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
    fn safe_command_matches_command_chain() {
        let rules = vec![
            SafeCommandRule {
                command_chain: vec!["git".to_string()],
                args: ArgPolicy::Any {
                    flags: HashMap::new(),
                    allow_positional: true,
                    positional_path_from: None,
                },
            },
            SafeCommandRule {
                command_chain: vec!["git".to_string(), "status".to_string()],
                args: ArgPolicy::AllowList {
                    flags: HashMap::new(),
                    allow_positional: true,
                    positional_path_from: None,
                },
            },
            SafeCommandRule {
                command_chain: vec!["cargo".to_string(), "check".to_string()],
                args: ArgPolicy::AllowList {
                    flags: HashMap::new(),
                    allow_positional: true,
                    positional_path_from: None,
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

    fn assert_unsafe_range_contains(command: &str, needle: &str) {
        let ranges = bash_unsafe_ranges(command);
        assert!(
            ranges
                .iter()
                .any(|range| command[range.clone()].contains(needle)),
            "expected unsafe range containing {needle:?}, got {ranges:?}"
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
        assert_unsafe_range_contains(command, "/etc/passwd");
    }

    #[test]
    fn bash_unsafe_ranges_marks_disallowed_token() {
        let command = "ls; rm -rf /";
        assert_unsafe_range_contains(command, ";");
    }

    #[test]
    fn bash_unsafe_ranges_marks_unknown_command() {
        let command = "rm -rf /";
        assert_unsafe_range_contains(command, "rm");
    }
}
