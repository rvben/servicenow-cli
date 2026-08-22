use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::api::ApiError;
use crate::credentials::{self, StoredCredential};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum AuthType {
    #[default]
    Basic,
    Bearer,
    OAuth,
}

impl AuthType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Bearer => "bearer",
            Self::OAuth => "oauth",
        }
    }
}

impl FromStr for AuthType {
    type Err = ApiError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "basic" => Ok(Self::Basic),
            "bearer" | "token" => Ok(Self::Bearer),
            "oauth" => Ok(Self::OAuth),
            _ => Err(ApiError::InvalidInput(format!(
                "invalid auth type '{value}'; expected basic, bearer, or oauth"
            ))),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ProfileConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_store: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,

    // Read legacy 0.1 files, but never write secrets back to disk.
    #[serde(default, skip_serializing)]
    password: Option<String>,
    #[serde(default, skip_serializing)]
    token: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct RawConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    active_profile: Option<String>,
    #[serde(default)]
    default: ProfileConfig,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    profiles: BTreeMap<String, ProfileConfig>,
}

#[derive(Clone, Debug)]
pub struct OAuthSession {
    pub refresh_token: Option<String>,
    pub expires_at: Option<u64>,
    pub client_secret: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub instance: String,
    pub username: Option<String>,
    pub secret: String,
    pub auth_type: AuthType,
    pub read_only: bool,
    pub profile: String,
    pub client_id: Option<String>,
    pub oauth_scope: Option<String>,
    pub redirect_uri: Option<String>,
    pub oauth: Option<OAuthSession>,
    credential_store: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileSummary {
    pub name: String,
    pub active: bool,
    pub instance: Option<String>,
    pub username: Option<String>,
    pub auth_type: String,
    pub read_only: bool,
    pub credential_store: String,
}

impl Config {
    pub fn load(
        instance_arg: Option<String>,
        username_arg: Option<String>,
        profile_arg: Option<String>,
    ) -> Result<Self, ApiError> {
        let file = load_file()?;
        let requested_profile = normalize(profile_arg)
            .or_else(|| env("SERVICENOW_PROFILE"))
            .or_else(|| normalize(file.active_profile.clone()));
        let profile_name = requested_profile.unwrap_or_else(|| "default".into());
        validate_profile_name(&profile_name)?;
        let file_profile = profile_from(&file, &profile_name)?.clone();

        let instance = normalize(instance_arg)
            .or_else(|| env("SERVICENOW_INSTANCE"))
            .or_else(|| normalize(file_profile.instance.clone()))
            .ok_or_else(|| {
                ApiError::InvalidInput(
                    "No ServiceNow instance configured. Run `servicenow setup`.".into(),
                )
            })?;
        let auth_type = env("SERVICENOW_AUTH_TYPE")
            .or_else(|| normalize(file_profile.auth_type.clone()))
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or_default();
        let username = normalize(username_arg)
            .or_else(|| env("SERVICENOW_USERNAME"))
            .or_else(|| normalize(file_profile.username.clone()));

        let env_secret = match auth_type {
            AuthType::Basic => env("SERVICENOW_PASSWORD"),
            AuthType::Bearer | AuthType::OAuth => env("SERVICENOW_TOKEN"),
        };
        let legacy_secret = match auth_type {
            AuthType::Basic => normalize(file_profile.password.clone()),
            AuthType::Bearer | AuthType::OAuth => normalize(file_profile.token.clone()),
        };
        let keychain_credential = if env_secret.is_none()
            && legacy_secret.is_none()
            && file_profile.credential_store.as_deref() == Some("keyring")
        {
            Some(credentials::load(&profile_name)?)
        } else {
            None
        };
        let secret = env_secret
            .or(legacy_secret)
            .or_else(|| {
                keychain_credential
                    .as_ref()
                    .map(|value| value.secret().into())
            })
            .ok_or_else(|| {
                ApiError::InvalidInput(match auth_type {
                    AuthType::Basic => "No password configured. Run `servicenow setup`.".into(),
                    AuthType::Bearer | AuthType::OAuth => {
                        "No access token configured. Run `servicenow setup`.".into()
                    }
                })
            })?;
        if matches!(auth_type, AuthType::Basic) && username.is_none() {
            return Err(ApiError::InvalidInput(
                "No username configured. Run `servicenow setup`.".into(),
            ));
        }
        let read_only = env("SERVICENOW_READ_ONLY")
            .map(|value| parse_bool(&value))
            .transpose()?
            .unwrap_or(file_profile.read_only.unwrap_or(false));
        let oauth = match keychain_credential {
            Some(StoredCredential::OAuth {
                refresh_token,
                expires_at,
                client_secret,
                ..
            }) => Some(OAuthSession {
                refresh_token,
                expires_at,
                client_secret,
            }),
            _ => None,
        };

        Ok(Self {
            instance,
            username,
            secret,
            auth_type,
            read_only,
            profile: profile_name,
            client_id: normalize(file_profile.client_id),
            oauth_scope: normalize(file_profile.oauth_scope),
            redirect_uri: normalize(file_profile.redirect_uri),
            oauth,
            credential_store: file_profile.credential_store.as_deref() == Some("keyring"),
        })
    }

    pub fn require_writable(&self) -> Result<(), ApiError> {
        if self.read_only {
            Err(ApiError::InvalidInput(format!(
                "write operation blocked: profile '{}' is read-only",
                self.profile
            )))
        } else {
            Ok(())
        }
    }

    pub fn uses_keychain(&self) -> bool {
        self.credential_store
    }
}

pub fn config_path() -> PathBuf {
    config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("config.toml")
}

pub fn cache_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("SERVICENOW_CACHE_DIR") {
        return PathBuf::from(path);
    }
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("servicenow")
}

pub fn init_document() -> serde_json::Value {
    let path = config_path();
    serde_json::json!({
        "configPath": path,
        "configExists": path.exists(),
        "example": {
            "active_profile": "work",
            "profiles": {
                "work": {
                    "instance": "company",
                    "username": "api-user",
                    "auth_type": "oauth",
                    "credential_store": "keyring",
                    "read_only": false
                }
            }
        },
        "setupCommand": "servicenow setup work",
        "secretStorage": "operating-system credential store"
    })
}

pub fn save_profile(name: &str, profile: ProfileConfig, make_active: bool) -> Result<(), ApiError> {
    validate_profile_name(name)?;
    let mut file = load_file()?;
    if name == "default" {
        file.default = profile;
    } else {
        file.profiles.insert(name.into(), profile);
    }
    if make_active {
        file.active_profile = Some(name.into());
    }
    save_file(&file)
}

pub fn profile_summaries() -> Result<Vec<ProfileSummary>, ApiError> {
    let file = load_file()?;
    let active = file.active_profile.as_deref().unwrap_or("default");
    let mut profiles = Vec::new();
    if file.default.instance.is_some() {
        profiles.push(summary("default", active, &file.default));
    }
    profiles.extend(
        file.profiles
            .iter()
            .map(|(name, profile)| summary(name, active, profile)),
    );
    Ok(profiles)
}

pub fn active_profile_name() -> Result<String, ApiError> {
    Ok(load_file()?
        .active_profile
        .unwrap_or_else(|| "default".into()))
}

pub fn use_profile(name: &str) -> Result<(), ApiError> {
    validate_profile_name(name)?;
    let mut file = load_file()?;
    profile_from(&file, name)?;
    file.active_profile = Some(name.into());
    save_file(&file)
}

pub fn remove_profile(name: &str) -> Result<bool, ApiError> {
    validate_profile_name(name)?;
    let mut file = load_file()?;
    let removed = if name == "default" {
        let existed = file.default.instance.is_some();
        file.default = ProfileConfig::default();
        existed
    } else {
        file.profiles.remove(name).is_some()
    };
    if removed {
        if file.active_profile.as_deref() == Some(name) {
            file.active_profile = None;
        }
        save_file(&file)?;
    }
    Ok(removed)
}

pub fn validate_profile_name(name: &str) -> Result<(), ApiError> {
    if !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(ApiError::InvalidInput(format!(
            "invalid profile name '{name}'; use letters, digits, '-' and '_'"
        )))
    }
}

fn profile_from<'a>(file: &'a RawConfig, name: &str) -> Result<&'a ProfileConfig, ApiError> {
    if name == "default" {
        return Ok(&file.default);
    }
    file.profiles.get(name).ok_or_else(|| {
        let available = file
            .profiles
            .keys()
            .map(String::as_str)
            .chain(file.default.instance.is_some().then_some("default"))
            .collect::<Vec<_>>()
            .join(", ");
        ApiError::NotFound(format!(
            "config profile '{name}'. Available: {}",
            if available.is_empty() {
                "none defined"
            } else {
                &available
            }
        ))
    })
}

fn summary(name: &str, active: &str, profile: &ProfileConfig) -> ProfileSummary {
    ProfileSummary {
        name: name.into(),
        active: name == active,
        instance: profile.instance.clone(),
        username: profile.username.clone(),
        auth_type: profile.auth_type.clone().unwrap_or_else(|| "basic".into()),
        read_only: profile.read_only.unwrap_or(false),
        credential_store: profile
            .credential_store
            .clone()
            .unwrap_or_else(|| "configuration/environment".into()),
    }
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

fn save_file(file: &RawConfig) -> Result<(), ApiError> {
    let path = config_path();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| {
        ApiError::Other(format!("failed to create {}: {error}", parent.display()))
    })?;
    let content = toml::to_string_pretty(file)
        .map_err(|error| ApiError::Other(format!("failed to encode config: {error}")))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| ApiError::Other(format!("failed to create config file: {error}")))?;
    temp.write_all(content.as_bytes())
        .map_err(|error| ApiError::Other(format!("failed to write config file: {error}")))?;
    temp.as_file()
        .sync_all()
        .map_err(|error| ApiError::Other(format!("failed to sync config file: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| ApiError::Other(format!("failed to protect config file: {error}")))?;
    }
    temp.persist(&path)
        .map_err(|error| ApiError::Other(format!("failed to replace config file: {error}")))?;
    Ok(())
}

fn config_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SERVICENOW_CONFIG_DIR").filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(path));
    }
    #[cfg(target_os = "windows")]
    {
        dirs::config_dir().map(|path| path.join("servicenow"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
            .map(|path| path.join("servicenow"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_profile_names() {
        assert!(validate_profile_name("production-eu").is_ok());
        assert!(validate_profile_name("team_alpha").is_ok());
        assert!(validate_profile_name("bad/profile").is_err());
    }

    #[test]
    fn new_profile_serialization_contains_no_secret_fields() {
        let profile = ProfileConfig {
            instance: Some("dev12345".into()),
            username: Some("admin".into()),
            auth_type: Some("basic".into()),
            credential_store: Some("keyring".into()),
            ..ProfileConfig::default()
        };
        let encoded = toml::to_string(&profile).unwrap();
        assert!(!encoded.contains("password"));
        assert!(!encoded.contains("token"));
        assert!(encoded.contains("credential_store"));
    }
}
