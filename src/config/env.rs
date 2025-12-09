use std::fmt::Debug;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Visitor};
use snafu::prelude::*;

use crate::Result;

#[derive(Clone)]
pub enum EnvString {
    String(String),
    EnvVar { name: String, value: Option<String> },
}

impl EnvString {
    pub fn get(&mut self) -> Result<&str> {
        match self {
            EnvString::EnvVar { name, value } => {
                if let Some(value) = value {
                    return Ok(value.as_str());
                } else {
                    let value_from_env = std::env::var(&name)
                        .whatever_context(format!("failed to get {name} from env"))?;
                    *value = Some(value_from_env);
                }
                Ok(value.as_ref().unwrap().as_str())
            }
            EnvString::String(value) => Ok(value),
        }
    }
}

impl Debug for EnvString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvString::String(value) => f.debug_tuple("EnvString::String").field(value).finish(),
            EnvString::EnvVar { name, .. } => f
                .debug_struct("EnvString::EnvVar")
                .field("name", name)
                .finish_non_exhaustive(),
        }
    }
}

impl Serialize for EnvString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            EnvString::String(value) => serializer.serialize_str(value),
            EnvString::EnvVar { name, .. } => serializer.serialize_str(&format!("${name}")),
        }
    }
}

struct EnvStringVisitor;

impl<'de> Visitor<'de> for EnvStringVisitor {
    type Value = EnvString;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a string value or a string value starting with $")
    }

    fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if let Some(v) = v.strip_prefix('$') {
            Ok(EnvString::EnvVar {
                name: v.to_string(),
                value: None,
            })
        } else {
            Ok(EnvString::String(v.to_string()))
        }
    }
}

impl<'de> Deserialize<'de> for EnvString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(EnvStringVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_string_get_string_variant() {
        let mut env_str = EnvString::String("direct_value".to_string());
        let value = env_str.get().unwrap();
        assert_eq!(value, "direct_value");
    }

    #[test]
    fn env_string_get_env_var_first_call() {
        unsafe {
            std::env::set_var("TEST_ENV_VAR", "env_value_123");
        }

        let mut env_str = EnvString::EnvVar {
            name: "TEST_ENV_VAR".to_string(),
            value: None,
        };

        let value = env_str.get().unwrap();
        assert_eq!(value, "env_value_123");

        unsafe {
            std::env::remove_var("TEST_ENV_VAR");
        }
    }

    #[test]
    fn env_string_get_env_var_with_cache() {
        unsafe {
            std::env::set_var("CACHE_TEST_VAR", "first_value");
        }

        let mut env_str = EnvString::EnvVar {
            name: "CACHE_TEST_VAR".to_string(),
            value: None,
        };

        let value1 = env_str.get().unwrap();
        assert_eq!(value1, "first_value");

        unsafe {
            std::env::set_var("CACHE_TEST_VAR", "second_value");
        }

        let value2 = env_str.get().unwrap();
        assert_eq!(value2, "first_value");

        unsafe {
            std::env::remove_var("CACHE_TEST_VAR");
        }
    }

    #[test]
    fn env_string_get_nonexistent_env_var() {
        unsafe {
            std::env::remove_var("NONEXISTENT_VAR_12345");
        }

        let mut env_str = EnvString::EnvVar {
            name: "NONEXISTENT_VAR_12345".to_string(),
            value: None,
        };

        let result = env_str.get();
        assert!(result.is_err());
    }

    // ========== Serialization Tests ==========

    #[test]
    fn env_string_serialize_string_variant() {
        let env_str = EnvString::String("direct_string".to_string());
        let serialized = serde_json::to_string(&env_str).unwrap();
        assert_eq!(serialized, "\"direct_string\"");
    }

    #[test]
    fn env_string_serialize_env_var_variant() {
        let env_str = EnvString::EnvVar {
            name: "PATH".to_string(),
            value: None,
        };
        let serialized = serde_json::to_string(&env_str).unwrap();
        assert_eq!(serialized, "\"$PATH\"");
    }

    #[test]
    fn env_string_deserialize_plain_string() {
        let json_str = "\"plain_value\"";
        let env_str: EnvString = serde_json::from_str(json_str).unwrap();

        match env_str {
            EnvString::String(s) => assert_eq!(s, "plain_value"),
            _ => panic!("Expected String variant"),
        }
    }

    #[test]
    fn env_string_deserialize_env_var_format() {
        let json_str = "\"$HOME\"";
        let env_str: EnvString = serde_json::from_str(json_str).unwrap();

        match env_str {
            EnvString::EnvVar { name, value: None } => {
                assert_eq!(name, "HOME");
            }
            _ => panic!("Expected EnvVar variant with None value"),
        }
    }

    #[test]
    fn env_string_deserialize_empty_string() {
        let json_str = "\"\"";
        let env_str: EnvString = serde_json::from_str(json_str).unwrap();

        match env_str {
            EnvString::String(s) => assert_eq!(s, ""),
            _ => panic!("Expected String variant"),
        }
    }
}
