use serde::{Deserialize, Serialize};

use crate::api::ApiError;

const SERVICE: &str = "servicenow-cli";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StoredCredential {
    Basic {
        password: String,
    },
    Bearer {
        access_token: String,
    },
    #[serde(rename = "oauth")]
    OAuth {
        access_token: String,
        refresh_token: Option<String>,
        expires_at: Option<u64>,
        client_secret: Option<String>,
    },
}

impl StoredCredential {
    pub fn secret(&self) -> &str {
        match self {
            Self::Basic { password } => password,
            Self::Bearer { access_token } | Self::OAuth { access_token, .. } => access_token,
        }
    }
}

pub fn store(profile: &str, credential: &StoredCredential) -> Result<(), ApiError> {
    let encoded = serde_json::to_string(credential)
        .map_err(|error| ApiError::Other(format!("failed to encode credential: {error}")))?;
    entry(profile)?
        .set_password(&encoded)
        .map_err(|error| keyring_error("store", error))
}

pub fn load(profile: &str) -> Result<StoredCredential, ApiError> {
    let encoded = entry(profile)?
        .get_password()
        .map_err(|error| keyring_error("read", error))?;
    serde_json::from_str(&encoded)
        .map_err(|error| ApiError::Other(format!("credential in keychain is invalid: {error}")))
}

pub fn delete(profile: &str) -> Result<bool, ApiError> {
    match entry(profile)?.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(error) => Err(keyring_error("delete", error)),
    }
}

pub fn available() -> Result<(), ApiError> {
    keyring::Entry::store_status()
        .as_ref()
        .map_err(|error| unavailable_error(&error.to_string()))
        .map(|_| ())
}

fn unavailable_error(detail: &str) -> ApiError {
    if detail.contains("org.freedesktop.secrets") && detail.contains("ServiceUnknown") {
        ApiError::InvalidInput(
            "OS credential store is unavailable: no Secret Service provider is running".into(),
        )
    } else {
        ApiError::InvalidInput(format!("OS credential store is unavailable: {detail}"))
    }
}

fn entry(profile: &str) -> Result<keyring::Entry, ApiError> {
    keyring::Entry::new(SERVICE, &format!("profile:{profile}"))
        .map_err(|error| keyring_error("open", error))
}

fn keyring_error(operation: &str, error: keyring::Error) -> ApiError {
    let message = match error {
        keyring::Error::NoEntry => {
            "credential not found; run `servicenow auth login` for this profile".to_string()
        }
        other => format!("failed to {operation} OS-keychain credential: {other}"),
    };
    ApiError::InvalidInput(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_credentials_never_expose_a_common_secret_field() {
        let credential = StoredCredential::OAuth {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at: Some(42),
            client_secret: Some("client".into()),
        };
        let encoded = serde_json::to_value(&credential).unwrap();
        assert_eq!(encoded["kind"], "oauth");
        assert!(encoded.get("secret").is_none());
        assert_eq!(credential.secret(), "access");
    }

    #[test]
    fn missing_linux_secret_service_has_a_human_error() {
        let error = unavailable_error(
            "Platform failure: zbus error: org.freedesktop.DBus.Error.ServiceUnknown: \
             The name org.freedesktop.secrets was not provided by any .service files",
        );
        assert_eq!(
            error.to_string(),
            "OS credential store is unavailable: no Secret Service provider is running"
        );
        assert!(!error.to_string().contains("zbus"));
    }
}
