mod env;
mod mcp;
mod provider;
mod ui;

use std::{
    collections::HashMap,
    error::Error as StdError,
    fmt,
    path::{Path, PathBuf},
};

pub use env::EnvString;
pub use mcp::{
    McpConfig, McpServerCommandConfig, McpServerConfig, McpServerConnection, McpServerHttpConfig,
};
pub use provider::{ProviderConfig, ProviderKind};
pub use ui::{MarkdownRenderEngine, UI};

type BoxError = Box<dyn StdError + Send + Sync>;

#[derive(Debug)]
struct OverrideError(String);

impl fmt::Display for OverrideError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl StdError for OverrideError {}

fn override_error(message: impl Into<String>) -> BoxError {
    Box::new(OverrideError(message.into()))
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct Config {
    pub ui: UI,
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub allow_tools: Option<Vec<String>>,
    #[serde(default)]
    pub deny_tools: Option<Vec<String>>,
    #[serde(default)]
    pub mcp: Option<McpConfig>,

    #[serde(skip)]
    pub config_dir: PathBuf,
}

impl Config {
    pub fn parse_file(path: &str) -> Result<Self, BoxError> {
        let content = std::fs::read_to_string(path)?;
        if path.ends_with(".toml") {
            let config: Config = toml::from_str(&content)?;
            Ok(config)
        } else {
            Err("Unsupported config file format".into())
        }
    }

    pub fn combo_dir(&self) -> PathBuf {
        self.config_dir.join("combos")
    }
}

pub fn default_config_dir() -> PathBuf {
    PathBuf::from(std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").expect("HOME environment variable not set");
        format!("{}/.config", home)
    }))
    .join("coco")
}

pub fn workspace_dir() -> PathBuf {
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut dir = current_dir.clone();
    loop {
        let git_dir = dir.join(".git");
        if git_dir.exists() && git_dir.is_dir() {
            return dir;
        }
        if let Some(parent) = dir.parent() {
            dir = parent.to_path_buf();
        } else {
            break;
        }
    }
    current_dir
}

pub fn workspace_config_path() -> PathBuf {
    workspace_dir().join(".coco").join("config.toml")
}

pub fn load_config_with_overrides(
    config_path: &Path,
    config_dir: &Path,
    workspace_override_path: Option<&Path>,
) -> Result<Config, BoxError> {
    let mut base_value = parse_toml_value(config_path)?;
    if let Some(path) = workspace_override_path
        && path.exists()
    {
        let override_value = parse_toml_value(path)?;
        merge_config_values(&mut base_value, override_value)?;
    }
    let merged = toml::to_string(&base_value)?;
    let mut config: Config = toml::from_str(&merged)?;
    config.config_dir = config_dir.to_path_buf();
    Ok(config)
}

fn parse_toml_value(path: &Path) -> Result<toml::Value, BoxError> {
    let content = std::fs::read_to_string(path)?;
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("toml") => Ok(toml::from_str(&content)?),
        _ => Err("Unsupported config file format".into()),
    }
}

fn merge_config_values(
    base: &mut toml::Value,
    override_value: toml::Value,
) -> Result<(), BoxError> {
    let base_table = base
        .as_table_mut()
        .ok_or_else(|| override_error("config must be a table"))?;
    let override_table = match override_value {
        toml::Value::Table(table) => table,
        _ => return Err(override_error("override config must be a table")),
    };
    merge_tables(base_table, override_table, "")
}

fn merge_tables(
    base: &mut toml::value::Table,
    override_table: toml::value::Table,
    path: &str,
) -> Result<(), BoxError> {
    let is_root = path.is_empty();
    for (key, override_value) in override_table {
        let next_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        if is_root && key == "providers" {
            merge_named_table_array(
                base,
                "providers",
                override_value,
                "providers",
                merge_provider,
            )?;
            continue;
        }
        if is_root && key == "mcp" {
            let override_table = value_as_table(override_value, &next_path)?;
            let base_entry = base
                .entry(key.clone())
                .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
            let base_table = base_entry
                .as_table_mut()
                .ok_or_else(|| override_error(format!("{next_path} must be a table")))?;
            merge_mcp_table(base_table, override_table, &next_path)?;
            continue;
        }
        if let Some(base_value) = base.get_mut(&key) {
            match (base_value, override_value) {
                (toml::Value::Table(base_table), toml::Value::Table(override_table)) => {
                    merge_tables(base_table, override_table, &next_path)?;
                }
                (base_value, override_value) => {
                    *base_value = override_value;
                }
            }
        } else {
            base.insert(key, override_value);
        }
    }
    Ok(())
}

fn merge_mcp_table(
    base: &mut toml::value::Table,
    override_table: toml::value::Table,
    path: &str,
) -> Result<(), BoxError> {
    for (key, override_value) in override_table {
        let next_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        if key == "servers" {
            merge_named_table_array(
                base,
                "servers",
                override_value,
                &next_path,
                merge_mcp_server,
            )?;
            continue;
        }
        if let Some(base_value) = base.get_mut(&key) {
            match (base_value, override_value) {
                (toml::Value::Table(base_table), toml::Value::Table(override_table)) => {
                    merge_tables(base_table, override_table, &next_path)?;
                }
                (base_value, override_value) => {
                    *base_value = override_value;
                }
            }
        } else {
            base.insert(key, override_value);
        }
    }
    Ok(())
}

fn merge_named_table_array(
    base: &mut toml::value::Table,
    key: &str,
    override_value: toml::Value,
    path: &str,
    merge_entry: fn(&mut toml::value::Table, toml::value::Table, &str) -> Result<(), BoxError>,
) -> Result<(), BoxError> {
    let override_items = match override_value {
        toml::Value::Array(items) => items,
        _ => return Err(override_error(format!("{path} must be an array"))),
    };
    let base_value = base
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let base_items = base_value
        .as_array_mut()
        .ok_or_else(|| override_error(format!("{path} must be an array")))?;
    let mut index_by_name = HashMap::new();
    for (idx, item) in base_items.iter().enumerate() {
        let name = table_name(item, path)?;
        if index_by_name.insert(name.clone(), idx).is_some() {
            return Err(override_error(format!("{path} has duplicated name {name}")));
        }
    }
    for item in override_items {
        let table = value_as_table(item, path)?;
        let name = table_name_from_table(&table, path)?;
        let entry_path = format!("{path}[{name}]");
        if let Some(&idx) = index_by_name.get(&name) {
            let base_table = base_items[idx]
                .as_table_mut()
                .ok_or_else(|| override_error(format!("{entry_path} must be a table")))?;
            merge_entry(base_table, table, &entry_path)?;
        } else {
            let mut new_table = toml::value::Table::new();
            merge_entry(&mut new_table, table, &entry_path)?;
            base_items.push(toml::Value::Table(new_table));
            index_by_name.insert(name, base_items.len() - 1);
        }
    }
    Ok(())
}

fn merge_provider(
    base: &mut toml::value::Table,
    override_table: toml::value::Table,
    path: &str,
) -> Result<(), BoxError> {
    merge_tables(base, override_table, path)
}

fn merge_mcp_server(
    base: &mut toml::value::Table,
    override_table: toml::value::Table,
    path: &str,
) -> Result<(), BoxError> {
    let has_url = override_table.contains_key("url");
    let has_command_fields = ["command", "args", "cwd", "env"]
        .iter()
        .any(|key| override_table.contains_key(*key));
    if has_url && has_command_fields {
        return Err(override_error(format!(
            "{path} cannot mix url with command fields"
        )));
    }
    if has_url {
        base.remove("command");
        base.remove("args");
        base.remove("cwd");
        base.remove("env");
    } else if has_command_fields {
        base.remove("url");
    }
    merge_tables(base, override_table, path)
}

fn value_as_table(value: toml::Value, path: &str) -> Result<toml::value::Table, BoxError> {
    match value {
        toml::Value::Table(table) => Ok(table),
        _ => Err(override_error(format!("{path} entry must be a table"))),
    }
}

fn table_name(value: &toml::Value, path: &str) -> Result<String, BoxError> {
    let table = value
        .as_table()
        .ok_or_else(|| override_error(format!("{path} entry must be a table")))?;
    table_name_from_table(table, path)
}

fn table_name_from_table(table: &toml::value::Table, path: &str) -> Result<String, BoxError> {
    match table.get("name") {
        Some(toml::Value::String(name)) if !name.trim().is_empty() => Ok(name.to_string()),
        _ => Err(override_error(format!("{path} entry missing name"))),
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, MarkdownRenderEngine, McpServerConnection, merge_config_values};

    fn base_config() -> String {
        [
            "[ui]",
            "markdown_render_engine = { type = \"native\" }",
            "",
            "[[providers]]",
            "name = \"default\"",
            "kind = \"anthropic\"",
            "api_key = \"test-key\"",
            "base_url = \"https://example.com\"",
            "",
        ]
        .join("\n")
    }

    #[test]
    fn parse_config_without_allow_tools_uses_default() {
        let config: Config = toml::from_str(&base_config()).expect("parse config");
        assert!(config.allow_tools.is_none());
        assert!(config.deny_tools.is_none());
        assert!(config.mcp.is_none());
    }

    #[test]
    fn parse_config_with_empty_allow_tools_disables_all() {
        let config_str = format!("allow_tools = []\n{}", base_config());
        let config: Config = toml::from_str(&config_str).expect("parse config");
        assert_eq!(config.allow_tools, Some(Vec::new()));
    }

    #[test]
    fn parse_config_with_empty_deny_tools_is_some() {
        let config_str = format!("deny_tools = []\n{}", base_config());
        let config: Config = toml::from_str(&config_str).expect("parse config");
        assert_eq!(config.deny_tools, Some(Vec::new()));
    }

    #[test]
    fn parse_config_with_mcp_defaults() {
        let config_str = format!(
            "[mcp]\n\n[[mcp.servers]]\nname = \"demo\"\ncommand = \"echo\"\n{}\n",
            base_config()
        );
        let config: Config = toml::from_str(&config_str).expect("parse config");
        let mcp = config.mcp.expect("mcp is present");
        assert_eq!(mcp.socket_path.to_string_lossy(), "coco-mcp.sock");
        assert_eq!(mcp.request_timeout_ms, 10_000);
        assert_eq!(mcp.idle_ttl_ms, 60_000);
        assert_eq!(mcp.servers.len(), 1);
        assert_eq!(mcp.servers[0].name, "demo");
        match &mcp.servers[0].connection {
            McpServerConnection::Command(command) => {
                assert_eq!(command.command, "echo");
            }
            McpServerConnection::Http(_) => {
                panic!("expected command server config");
            }
        }
    }

    #[test]
    fn parse_config_with_mcp_http_server() {
        let config_str = format!(
            "[mcp]\n\n[[mcp.servers]]\nname = \"remote\"\nurl = \"http://localhost:8080/mcp\"\n{}\n",
            base_config()
        );
        let config: Config = toml::from_str(&config_str).expect("parse config");
        let mcp = config.mcp.expect("mcp is present");
        assert_eq!(mcp.servers.len(), 1);
        match &mcp.servers[0].connection {
            McpServerConnection::Http(http) => {
                assert_eq!(http.url, "http://localhost:8080/mcp");
            }
            McpServerConnection::Command(_) => {
                panic!("expected http server config");
            }
        }
    }

    #[test]
    fn merge_config_with_workspace_overrides() {
        let config_str = format!(
            "[mcp]\nrequest_timeout_ms = 1000\n\n[[mcp.servers]]\nname = \"demo\"\ncommand = \"echo\"\n{}\n",
            base_config()
        );
        let mut base_value: toml::Value = toml::from_str(&config_str).expect("parse config");
        let override_str = [
            "allow_tools = [\"bash\"]",
            "",
            "[ui]",
            "theme = \"nord\"",
            "",
            "[[providers]]",
            "name = \"default\"",
            "base_url = \"https://override.example\"",
            "",
            "[[providers]]",
            "name = \"new\"",
            "kind = \"open_a_i\"",
            "api_key = \"test-key\"",
            "base_url = \"https://new.example\"",
            "",
            "[mcp]",
            "request_timeout_ms = 2000",
            "",
            "[[mcp.servers]]",
            "name = \"demo\"",
            "args = [\"--flag\"]",
            "",
            "[[mcp.servers]]",
            "name = \"extra\"",
            "url = \"http://localhost:1234/mcp\"",
        ]
        .join("\n");
        let override_value: toml::Value = toml::from_str(&override_str).expect("parse override");
        merge_config_values(&mut base_value, override_value).expect("merge override");
        let merged = toml::to_string(&base_value).expect("format config");
        let config: Config = toml::from_str(&merged).expect("parse config");

        assert_eq!(config.ui.theme, "nord");
        assert!(matches!(
            config.ui.markdown_render_engine,
            MarkdownRenderEngine::Native
        ));
        assert_eq!(config.allow_tools, Some(vec!["bash".to_string()]));
        assert_eq!(config.providers.len(), 2);
        let default_provider = config
            .providers
            .iter()
            .find(|provider| provider.name == "default")
            .expect("default provider");
        assert_eq!(default_provider.base_url, "https://override.example");
        let new_provider = config
            .providers
            .iter()
            .find(|provider| provider.name == "new")
            .expect("new provider");
        assert_eq!(new_provider.base_url, "https://new.example");

        let mcp = config.mcp.expect("mcp config");
        assert_eq!(mcp.request_timeout_ms, 2000);
        assert_eq!(mcp.servers.len(), 2);
        let demo = mcp
            .servers
            .iter()
            .find(|server| server.name == "demo")
            .expect("demo server");
        match &demo.connection {
            McpServerConnection::Command(command) => {
                assert_eq!(command.command, "echo");
                assert_eq!(command.args, vec!["--flag"]);
            }
            McpServerConnection::Http(_) => {
                panic!("expected command server config");
            }
        }
        let extra = mcp
            .servers
            .iter()
            .find(|server| server.name == "extra")
            .expect("extra server");
        match &extra.connection {
            McpServerConnection::Http(http) => {
                assert_eq!(http.url, "http://localhost:1234/mcp");
            }
            McpServerConnection::Command(_) => {
                panic!("expected http server config");
            }
        }
    }

    #[test]
    fn merge_config_rejects_new_provider_missing_fields() {
        let mut base_value: toml::Value = toml::from_str(&base_config()).expect("parse config");
        let override_str = ["[[providers]]", "name = \"new\""].join("\n");
        let override_value: toml::Value = toml::from_str(&override_str).expect("parse override");
        merge_config_values(&mut base_value, override_value).expect("merge override");
        let merged = toml::to_string(&base_value).expect("format config");
        let parsed: Result<Config, _> = toml::from_str(&merged);
        assert!(parsed.is_err());
    }

    #[test]
    fn merge_config_rejects_mcp_server_mixed_url_and_command() {
        let config_str = format!(
            "[mcp]\n\n[[mcp.servers]]\nname = \"demo\"\ncommand = \"echo\"\n{}\n",
            base_config()
        );
        let mut base_value: toml::Value = toml::from_str(&config_str).expect("parse config");
        let override_str = [
            "[[mcp.servers]]",
            "name = \"demo\"",
            "command = \"echo\"",
            "url = \"http://localhost:8080/mcp\"",
        ]
        .join("\n");
        let override_value: toml::Value = toml::from_str(&override_str).expect("parse override");
        assert!(merge_config_values(&mut base_value, override_value).is_err());
    }
}
