use std::time::{Duration, SystemTime, UNIX_EPOCH};

use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl,
    RefreshToken, Scope, TokenResponse, TokenUrl,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::api::{ApiError, normalize_instance};
use crate::config::{AuthType, Config};
use crate::credentials::{self, StoredCredential};

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

pub async fn oauth_login(
    instance: &str,
    client_id: &str,
    client_secret: Option<String>,
    scope: Option<&str>,
    redirect_uri: &str,
    open_browser: bool,
) -> Result<StoredCredential, ApiError> {
    let site_url = normalize_instance(instance)?;
    let redirect = reqwest::Url::parse(redirect_uri)
        .map_err(|error| ApiError::InvalidInput(format!("invalid OAuth redirect URI: {error}")))?;
    if redirect.scheme() != "http"
        || !matches!(redirect.host_str(), Some("127.0.0.1" | "localhost"))
        || redirect.port().is_none()
    {
        return Err(ApiError::InvalidInput(
            "OAuth redirect URI must use http://127.0.0.1:<port>/path or localhost".into(),
        ));
    }
    let port = redirect.port().expect("validated redirect port");
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|error| {
            ApiError::Other(format!(
                "failed to listen for OAuth callback on port {port}: {error}"
            ))
        })?;

    let client = oauth_client(&site_url, client_id, client_secret.as_deref(), redirect_uri)?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let mut authorization = client
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(challenge);
    if let Some(scope) = scope.filter(|value| !value.trim().is_empty()) {
        for value in scope.split_whitespace() {
            authorization = authorization.add_scope(Scope::new(value.into()));
        }
    }
    let (authorization_url, csrf) = authorization.url();

    eprintln!("Authorize this CLI in your browser:\n\n  {authorization_url}\n");
    if open_browser && let Err(error) = open::that(authorization_url.as_str()) {
        eprintln!("Could not open the browser automatically: {error}");
    }

    let (code, returned_state) = tokio::time::timeout(
        CALLBACK_TIMEOUT,
        receive_callback(listener, redirect.path()),
    )
    .await
    .map_err(|_| ApiError::Auth("OAuth authorization timed out after five minutes".into()))??;
    if returned_state != csrf.secret().as_str() {
        return Err(ApiError::Auth(
            "OAuth state mismatch; authorization was rejected for safety".into(),
        ));
    }

    let http = oauth2::reqwest::ClientBuilder::new()
        .redirect(oauth2::reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| ApiError::Other(format!("failed to build OAuth client: {error}")))?;
    let token = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(verifier)
        .request_async(&http)
        .await
        .map_err(|error| ApiError::Auth(format!("OAuth token exchange failed: {error}")))?;
    Ok(StoredCredential::OAuth {
        access_token: token.access_token().secret().into(),
        refresh_token: token.refresh_token().map(|value| value.secret().into()),
        expires_at: token.expires_in().map(expires_at),
        client_secret,
    })
}

pub async fn refresh_if_needed(config: &mut Config) -> Result<bool, ApiError> {
    if !matches!(config.auth_type, AuthType::OAuth) || !config.uses_keychain() {
        return Ok(false);
    }
    let Some(session) = config.oauth.clone() else {
        return Ok(false);
    };
    if session
        .expires_at
        .is_none_or(|expiry| expiry > epoch_seconds().saturating_add(60))
    {
        return Ok(false);
    }
    let Some(refresh_token) = session.refresh_token else {
        return Err(ApiError::Auth(
            "OAuth token expired and no refresh token is available; run `servicenow auth login`"
                .into(),
        ));
    };
    let client_id = config.client_id.as_deref().ok_or_else(|| {
        ApiError::InvalidInput("OAuth profile is missing client_id; log in again".into())
    })?;
    let redirect_uri = config
        .redirect_uri
        .as_deref()
        .unwrap_or("http://127.0.0.1:8484/callback");
    let site_url = normalize_instance(&config.instance)?;
    let client = oauth_client(
        &site_url,
        client_id,
        session.client_secret.as_deref(),
        redirect_uri,
    )?;
    let http = oauth2::reqwest::ClientBuilder::new()
        .redirect(oauth2::reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| ApiError::Other(format!("failed to build OAuth client: {error}")))?;
    let token = client
        .exchange_refresh_token(&RefreshToken::new(refresh_token.clone()))
        .request_async(&http)
        .await
        .map_err(|error| ApiError::Auth(format!("OAuth refresh failed: {error}")))?;
    let access_token = token.access_token().secret().to_string();
    let new_refresh = token
        .refresh_token()
        .map(|value| value.secret().to_string())
        .or(Some(refresh_token));
    let new_expiry = token.expires_in().map(expires_at);
    credentials::store(
        &config.profile,
        &StoredCredential::OAuth {
            access_token: access_token.clone(),
            refresh_token: new_refresh.clone(),
            expires_at: new_expiry,
            client_secret: session.client_secret.clone(),
        },
    )?;
    config.secret = access_token;
    config.oauth = Some(crate::config::OAuthSession {
        refresh_token: new_refresh,
        expires_at: new_expiry,
        client_secret: session.client_secret,
    });
    Ok(true)
}

fn oauth_client(
    site_url: &str,
    client_id: &str,
    client_secret: Option<&str>,
    redirect_uri: &str,
) -> Result<
    BasicClient<
        oauth2::EndpointSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointSet,
    >,
    ApiError,
> {
    let mut client = BasicClient::new(ClientId::new(client_id.into()))
        .set_auth_uri(
            AuthUrl::new(format!("{site_url}/oauth_auth.do"))
                .map_err(|error| ApiError::InvalidInput(format!("invalid OAuth URL: {error}")))?,
        )
        .set_token_uri(
            TokenUrl::new(format!("{site_url}/oauth_token.do"))
                .map_err(|error| ApiError::InvalidInput(format!("invalid token URL: {error}")))?,
        )
        .set_redirect_uri(RedirectUrl::new(redirect_uri.into()).map_err(|error| {
            ApiError::InvalidInput(format!("invalid OAuth redirect URI: {error}"))
        })?);
    if let Some(secret) = client_secret.filter(|value| !value.is_empty()) {
        client = client.set_client_secret(ClientSecret::new(secret.into()));
    }
    Ok(client)
}

async fn receive_callback(
    listener: TcpListener,
    expected_path: &str,
) -> Result<(String, String), ApiError> {
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|error| ApiError::Other(format!("OAuth callback failed: {error}")))?;
    let mut buffer = vec![0_u8; 16 * 1024];
    let count = stream
        .read(&mut buffer)
        .await
        .map_err(|error| ApiError::Other(format!("failed to read OAuth callback: {error}")))?;
    let request = String::from_utf8_lossy(&buffer[..count]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| ApiError::Auth("invalid OAuth callback request".into()))?;
    let callback = reqwest::Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|error| ApiError::Auth(format!("invalid OAuth callback: {error}")))?;
    if callback.path() != expected_path {
        return Err(ApiError::Auth("OAuth callback path did not match".into()));
    }
    let params: std::collections::HashMap<_, _> = callback.query_pairs().into_owned().collect();
    let result = if let Some(error) = params.get("error") {
        Err(ApiError::Auth(format!(
            "OAuth authorization was denied: {}",
            params.get("error_description").unwrap_or(error)
        )))
    } else {
        let code = params
            .get("code")
            .cloned()
            .ok_or_else(|| ApiError::Auth("OAuth callback omitted code".into()))?;
        let state = params
            .get("state")
            .cloned()
            .ok_or_else(|| ApiError::Auth("OAuth callback omitted state".into()))?;
        Ok((code, state))
    };
    let (status, message) = if result.is_ok() {
        ("200 OK", "Authorization complete. You can close this tab.")
    } else {
        (
            "400 Bad Request",
            "Authorization failed. Return to your terminal.",
        )
    };
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>ServiceNow CLI</title><style>body{{font:18px system-ui;max-width:42rem;margin:15vh auto;padding:2rem;color:#172b4d}}strong{{color:#2e844a}}</style><strong>{message}</strong>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    result
}

fn expires_at(duration: Duration) -> u64 {
    epoch_seconds().saturating_add(duration.as_secs())
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_loopback_oauth_redirects() {
        let error = oauth_login(
            "dev12345",
            "client",
            None,
            None,
            "https://example.com/callback",
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ApiError::InvalidInput(_)));
    }
}
