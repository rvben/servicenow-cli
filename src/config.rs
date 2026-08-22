use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::api::ApiError;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthType {
    #[default]
    Basic,
    Bearer,
}

impl FromStr for AuthType {
    type Err = ApiError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "basic" => Ok(Self::Basic),
            "bearer" | "oauth" => Ok(Self::Bearer),
            _ => Err(ApiError::InvalidInput(format!(
                "invalid auth type '{value}'; expected basic or bearer"
            ))),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ProfileConfig {
    instance: Option<String>,
    username: Option<String>,
    password: Option<String>,
    token: Option<String>,
    auth_type: Option<String>,
    read_only: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    default: ProfileConfig,
    #[serde(default)]
    profiles: BTreeMap<String, ProfileConfig>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub instance: String,
    pub username: Option<String>,
    pub secret: String,
    pub auth_type: AuthType,
    pub read_only: bool,
    pub profile: Option<String>,
}

impl Config {
    pub fn load(
        instance_arg: Option<String>,
        username_arg: Option<String>,
        profile_arg: Option<String>,
    ) -> Result<Self, ApiError> {
        let profile_name = normalize(profile_arg).or_else(|| env("SERVICENOW_PROFILE"));
        let file = load_file()?;
        let file_profile = match profile_name.as_deref() {
            Some(name) => file.profiles.get(name).cloned().ok_or_else(|| {
                let available = if file.profiles.is_empty() {
                    "none defined".into()
                } else {
                    file.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
                };
                ApiError::NotFound(format!("config profile '{name}'. Available: {available}"))
            })?,
            None => file.default,
        };

        let instance = normalize(instance_arg)
            .or_else(|| env("SERVICENOW_INSTANCE"))
            .or_else(|| normalize(file_profile.instance))
            .ok_or_else(|| {
                ApiError::InvalidInput(
                    "No ServiceNow instance configured. Set SERVICENOW_INSTANCE or run `servicenow config init`.".into(),
                )
            })?;
        let auth_type = env("SERVICENOW_AUTH_TYPE")
            .or_else(|| normalize(file_profile.auth_type))
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or_default();
        let username = normalize(username_arg)
            .or_else(|| env("SERVICENOW_USERNAME"))
            .or_else(|| normalize(file_profile.username));
        let secret = match auth_type {
            AuthType::Basic => env("SERVICENOW_PASSWORD")
                .or_else(|| normalize(file_profile.password))
                .ok_or_else(|| ApiError::InvalidInput(
                    "No password configured. Set SERVICENOW_PASSWORD or run `servicenow config init`.".into(),
                ))?,
            AuthType::Bearer => env("SERVICENOW_TOKEN")
                .or_else(|| normalize(file_profile.token))
                .ok_or_else(|| ApiError::InvalidInput(
                    "No OAuth access token configured. Set SERVICENOW_TOKEN or run `servicenow config init`.".into(),
                ))?,
        };
        if matches!(auth_type, AuthType::Basic) && username.is_none() {
            return Err(ApiError::InvalidInput(
                "No username configured. Set SERVICENOW_USERNAME or run `servicenow config init`."
                    .into(),
            ));
        }
        let read_only = env("SERVICENOW_READ_ONLY")
            .map(|value| parse_bool(&value))
            .transpose()?
            .unwrap_or(file_profile.read_only.unwrap_or(false));

        Ok(Self {
            instance,
            username,
            secret,
            auth_type,
            read_only,
            profile: profile_name,
        })
    }

    pub fn require_writable(&self) -> Result<(), ApiError> {
        if self.read_only {
            Err(ApiError::InvalidInput(
                "write operation blocked by SERVICENOW_READ_ONLY/config read_only".into(),
            ))
        } else {
            Ok(())
        }
    }
}

pub fn config_path() -> PathBuf {
    config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("servicenow")
        .join("config.toml")
}

pub fn init_document() -> serde_json::Value {
    let path = config_path();
    serde_json::json!({
        "configPath": path,
        "configExists": path.exists(),
        "example": {
            "default": {
                "instance": "dev12345",
                "username": "admin",
                "password": "your-password",
                "auth_type": "basic",
                "read_only": false
            }
        },
        "recommendedPermissions": format!("chmod 600 {}", path.display())
    })
}

fn load_file() -> Result<RawConfig, ApiError> {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(content) => toml::from_str(&content).map_err(|error| {
            ApiError::InvalidInput(format!("failed to parse {}: {error}", path.display()))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(RawConfig::default()),
        Err(error) => Err(ApiError::Other(format!(
            "failed to read {}: {error}",
            path.display()
        ))),
    }
}

fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        dirs::config_dir()
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .and_then(|value| normalize(Some(value)))
}

fn normalize(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn parse_bool(value: &str) -> Result<bool, ApiError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ApiError::InvalidInput(format!(
            "invalid boolean '{value}'; expected true/false, yes/no, on/off, or 1/0"
        ))),
    }
}
