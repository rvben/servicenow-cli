use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::HeaderValue;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::api::{ApiError, normalize_instance};
use crate::cookies::BrowserCookies;
use crate::credentials::StoredCredential;

const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const BROWSER_START_TIMEOUT: Duration = Duration::from_secs(20);
const SESSION_CHECK_INTERVAL: Duration = Duration::from_millis(750);
const USER_TOKEN_EXPRESSION: &str = r#"(() => {
    const seen = new Set();
    const readToken = (view) => {
        if (!view || seen.has(view)) return '';
        seen.add(view);
        try {
            if (typeof view.g_ck === 'string' && view.g_ck) return view.g_ck;
            const document = view.document;
            const field = document && (document.getElementById('sysparm_ck') || document.querySelector('input[name="sysparm_ck"]'));
            if (field && typeof field.value === 'string' && field.value) return field.value;
        } catch (_) {}
        try {
            for (let index = 0; index < view.frames.length; index++) {
                const token = readToken(view.frames[index]);
                if (token) return token;
            }
        } catch (_) {}
        return '';
    };
    return readToken(window);
})()"#;

/// A secret-free milestone reached during browser-session sign-in.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BrowserProgress {
    StartingPrivateBrowser,
    PrivateBrowserOpened,
    WaitingForBrowserChannel,
    BrowserChannelReady,
    WaitingForServiceNowPage,
    ServiceNowPageDetected,
    ReadingSessionCookies,
    SessionCookiesDetected,
    ReadingUserToken,
    UserTokenDetected,
    ValidatingSession,
    ValidationResponse(u16),
    SessionValidated,
}

impl BrowserProgress {
    /// Human-readable text that never contains browser or ServiceNow secrets.
    pub fn message(self) -> String {
        match self {
            Self::StartingPrivateBrowser => "starting a private browser".into(),
            Self::PrivateBrowserOpened => "private browser opened".into(),
            Self::WaitingForBrowserChannel => {
                "waiting for the localhost-only browser sign-in channel".into()
            }
            Self::BrowserChannelReady => "browser sign-in channel is ready".into(),
            Self::WaitingForServiceNowPage => "waiting for an authenticated ServiceNow page".into(),
            Self::ServiceNowPageDetected => "authenticated ServiceNow page detected".into(),
            Self::ReadingSessionCookies => "checking ServiceNow-scoped session cookies".into(),
            Self::SessionCookiesDetected => "ServiceNow session cookies detected".into(),
            Self::ReadingUserToken => "looking for the ServiceNow user token".into(),
            Self::UserTokenDetected => "ServiceNow user token detected".into(),
            Self::ValidatingSession => "validating the session with the Table API".into(),
            Self::ValidationResponse(status) => {
                format!("Table API validation returned HTTP {status}; continuing to wait")
            }
            Self::SessionValidated => "ServiceNow browser session validated".into(),
        }
    }
}

struct ProgressReporter<'a> {
    callback: &'a mut dyn FnMut(BrowserProgress),
    reported: HashSet<BrowserProgress>,
}

impl ProgressReporter<'_> {
    fn report(&mut self, progress: BrowserProgress) {
        if self.reported.insert(progress) {
            (self.callback)(progress);
        }
    }
}

/// Sign in through an isolated Chromium profile and retain only cookies scoped
/// to the requested ServiceNow instance.
pub async fn browser_login(
    instance: &str,
    open_browser: bool,
) -> Result<StoredCredential, ApiError> {
    browser_login_with_progress(instance, open_browser, |_| {}).await
}

/// Sign in through a browser and report allowlisted, secret-free progress.
pub async fn browser_login_with_progress(
    instance: &str,
    open_browser: bool,
    mut callback: impl FnMut(BrowserProgress),
) -> Result<StoredCredential, ApiError> {
    if !open_browser {
        return Err(ApiError::InvalidInput(
            "browser-session sign-in must open a browser; remove --no-browser or choose another --method"
                .into(),
        ));
    }
    let site_url = normalize_instance(instance)?;
    let mut progress = ProgressReporter {
        callback: &mut callback,
        reported: HashSet::new(),
    };

    let session = match select_browser_backend()? {
        BrowserBackend::Native(browser) => {
            native_browser_cookie_with_progress(&site_url, browser, &mut progress).await?
        }
        BrowserBackend::WindowsBridge(browser) => {
            windows_browser_cookie(&site_url, browser.as_deref(), &mut progress).await?
        }
    };
    HeaderValue::from_str(&session.cookie)
        .map_err(|_| ApiError::Other("the browser returned an invalid session cookie".into()))?;
    HeaderValue::from_str(&session.user_token)
        .map_err(|_| ApiError::Other("the browser returned an invalid user token".into()))?;
    Ok(StoredCredential::Browser {
        cookie: session.cookie,
        user_token: session.user_token,
    })
}

#[derive(serde::Deserialize)]
struct BrowserSession {
    cookie: String,
    user_token: String,
}

async fn native_browser_cookie_with_progress(
    site_url: &str,
    browser: PathBuf,
    progress: &mut ProgressReporter<'_>,
) -> Result<BrowserSession, ApiError> {
    let private_mode = private_browsing_argument(&browser);
    let profile = tempfile::tempdir()
        .map_err(|error| ApiError::Other(format!("failed to create browser profile: {error}")))?;
    let login_url = format!("{site_url}/nav_to.do?uri=incident_list.do");
    progress.report(BrowserProgress::StartingPrivateBrowser);
    let child = std::process::Command::new(&browser)
        .args([
            "--remote-debugging-port=0".into(),
            "--remote-debugging-address=127.0.0.1".into(),
            format!("--user-data-dir={}", profile.path().display()),
            "--no-first-run".into(),
            "--no-default-browser-check".into(),
            "--disable-sync".into(),
            private_mode.into(),
            "--new-window".into(),
            login_url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            ApiError::Other(format!(
                "failed to start browser {}: {error}",
                browser.display()
            ))
        })?;
    progress.report(BrowserProgress::PrivateBrowserOpened);
    let mut process = NativeBrowser {
        child,
        _profile: profile,
    };
    progress.report(BrowserProgress::WaitingForBrowserChannel);
    let websocket_url = wait_for_debugger(&mut process).await?;
    progress.report(BrowserProgress::BrowserChannelReady);
    wait_for_session_cookie_with_progress(&websocket_url, site_url, Some(&mut process), progress)
        .await
}

#[cfg(test)]
async fn native_browser_cookie(site_url: &str) -> Result<BrowserSession, ApiError> {
    let mut callback = |_| {};
    let mut progress = ProgressReporter {
        callback: &mut callback,
        reported: HashSet::new(),
    };
    let browser = find_native_browser(std::env::var_os("SERVICENOW_BROWSER").as_deref())?;
    native_browser_cookie_with_progress(site_url, browser, &mut progress).await
}

fn private_browsing_argument(browser: &Path) -> &'static str {
    let executable = browser
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if executable.contains("msedge") || executable.contains("microsoft-edge") {
        "--inprivate"
    } else {
        "--incognito"
    }
}

struct NativeBrowser {
    child: Child,
    _profile: tempfile::TempDir,
}

impl NativeBrowser {
    fn ensure_running(&mut self) -> Result<(), ApiError> {
        match self.child.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => Err(ApiError::Other(format!(
                "browser closed before ServiceNow sign-in completed ({status})"
            ))),
            Err(error) => Err(ApiError::Other(format!(
                "failed to inspect browser process: {error}"
            ))),
        }
    }
}

impl Drop for NativeBrowser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn wait_for_debugger(process: &mut NativeBrowser) -> Result<String, ApiError> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let started = Instant::now();
    loop {
        process.ensure_running()?;
        if let Ok(active_port) =
            std::fs::read_to_string(process._profile.path().join("DevToolsActivePort"))
            && let Some(port) = active_port.lines().next()
            && port.parse::<u16>().is_ok()
        {
            let endpoint = format!("http://127.0.0.1:{port}/json/version");
            if let Ok(response) = http.get(&endpoint).send().await
                && let Ok(document) = response.json::<Value>().await
                && let Some(url) = document.get("webSocketDebuggerUrl").and_then(Value::as_str)
            {
                return Ok(url.into());
            }
        }
        if started.elapsed() >= BROWSER_START_TIMEOUT {
            return Err(ApiError::Other(
                "browser started but its private sign-in channel did not become available; set SERVICENOW_BROWSER to Chrome, Edge, or Chromium"
                    .into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

type CdpSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn wait_for_session_cookie_with_progress(
    websocket_url: &str,
    site_url: &str,
    mut process: Option<&mut NativeBrowser>,
    progress: &mut ProgressReporter<'_>,
) -> Result<BrowserSession, ApiError> {
    let (mut socket, _) = connect_async(websocket_url)
        .await
        .map_err(|error| ApiError::Other(format!("failed to connect to browser: {error}")))?;
    let origin = reqwest::Url::parse(site_url)
        .map_err(|error| ApiError::InvalidInput(format!("invalid instance URL: {error}")))?;
    let host = origin
        .host_str()
        .ok_or_else(|| ApiError::InvalidInput("instance URL has no hostname".into()))?;
    let is_https = origin.scheme() == "https";
    let started = Instant::now();
    let mut id = 0_u64;
    let mut last_stage = "the authenticated ServiceNow page";
    progress.report(BrowserProgress::WaitingForServiceNowPage);

    loop {
        if let Some(process) = process.as_deref_mut() {
            process.ensure_running()?;
        }
        id += 1;
        let targets = cdp_command(&mut socket, id, "Target.getTargets", json!({}), None).await?;
        let Some(target) = service_now_page_target(&targets, host) else {
            if started.elapsed() >= LOGIN_TIMEOUT {
                return Err(ApiError::Auth(browser_timeout_message(last_stage)));
            }
            tokio::time::sleep(SESSION_CHECK_INTERVAL).await;
            continue;
        };
        progress.report(BrowserProgress::ServiceNowPageDetected);
        last_stage = "ServiceNow session cookies";
        progress.report(BrowserProgress::ReadingSessionCookies);
        id += 1;
        let attached = cdp_command(
            &mut socket,
            id,
            "Target.attachToTarget",
            json!({"targetId": target.target_id, "flatten": true}),
            None,
        )
        .await?;
        let session_id = attached
            .pointer("/result/sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ApiError::Other("browser did not open the private ServiceNow page session".into())
            })?
            .to_string();
        id += 1;
        let response = cdp_command(
            &mut socket,
            id,
            "Network.getCookies",
            json!({"urls": [format!("{site_url}/api/now/")]}),
            Some(&session_id),
        )
        .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                detach_page_session(&mut socket, &mut id, &session_id).await;
                return Err(error);
            }
        };
        let completed_session: Result<Option<BrowserSession>, ApiError> = async {
            if let Some(cookie) = service_now_cookie_header(&response, host, is_https) {
                progress.report(BrowserProgress::SessionCookiesDetected);
                last_stage = "the ServiceNow user token (`g_ck` or `sysparm_ck`)";
                progress.report(BrowserProgress::ReadingUserToken);
                if let Some(user_token) =
                    service_now_user_token(&mut socket, &mut id, &session_id).await?
                {
                    progress.report(BrowserProgress::UserTokenDetected);
                    last_stage = "ServiceNow REST session validation";
                    progress.report(BrowserProgress::ValidatingSession);
                    match validate_session(site_url, &cookie, &user_token).await? {
                        SessionValidation::Authenticated(cookie) => {
                            progress.report(BrowserProgress::SessionValidated);
                            return Ok(Some(BrowserSession { cookie, user_token }));
                        }
                        SessionValidation::Waiting(status) => {
                            progress.report(BrowserProgress::ValidationResponse(status));
                        }
                    }
                }
            }
            Ok(None)
        }
        .await;
        detach_page_session(&mut socket, &mut id, &session_id).await;
        if let Some(session) = completed_session? {
            return Ok(session);
        }
        if started.elapsed() >= LOGIN_TIMEOUT {
            return Err(ApiError::Auth(browser_timeout_message(last_stage)));
        }
        tokio::time::sleep(SESSION_CHECK_INTERVAL).await;
    }
}

fn browser_timeout_message(stage: &str) -> String {
    format!(
        "browser sign-in timed out after five minutes while waiting for {stage}; no credential was stored"
    )
}

struct BrowserTarget {
    target_id: String,
}

fn service_now_page_target(document: &Value, host: &str) -> Option<BrowserTarget> {
    document
        .pointer("/result/targetInfos")?
        .as_array()?
        .iter()
        .find_map(|target| {
            let is_page = target.get("type").and_then(Value::as_str) == Some("page");
            let url = target.get("url").and_then(Value::as_str)?;
            let target_host = reqwest::Url::parse(url).ok()?.host_str()?.to_string();
            (is_page && target_host.eq_ignore_ascii_case(host)).then(|| BrowserTarget {
                target_id: target
                    .get("targetId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .filter(|target| !target.target_id.is_empty())
}

async fn cdp_command(
    socket: &mut CdpSocket,
    id: u64,
    method: &str,
    params: Value,
    session_id: Option<&str>,
) -> Result<Value, ApiError> {
    let mut request = json!({"id": id, "method": method, "params": params});
    if let Some(session_id) = session_id {
        request["sessionId"] = Value::String(session_id.into());
    }
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(|error| ApiError::Other(format!("failed to query browser session: {error}")))?;
    while let Some(message) = socket.next().await {
        match message
            .map_err(|error| ApiError::Other(format!("browser session channel failed: {error}")))?
        {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(text.as_str()).map_err(|error| {
                    ApiError::Other(format!("browser returned invalid session data: {error}"))
                })?;
                if value.get("id").and_then(Value::as_u64) == Some(id) {
                    if let Some(error) = value.get("error") {
                        return Err(ApiError::Other(format!(
                            "browser rejected the session query: {error}"
                        )));
                    }
                    return Ok(value);
                }
            }
            Message::Ping(data) => socket.send(Message::Pong(data)).await.map_err(|error| {
                ApiError::Other(format!("browser session channel failed: {error}"))
            })?,
            Message::Close(_) => {
                return Err(ApiError::Other(
                    "browser closed its private sign-in channel".into(),
                ));
            }
            _ => {}
        }
    }
    Err(ApiError::Other(
        "browser closed its private sign-in channel".into(),
    ))
}

async fn service_now_user_token(
    socket: &mut CdpSocket,
    id: &mut u64,
    session_id: &str,
) -> Result<Option<String>, ApiError> {
    *id += 1;
    let evaluated = cdp_command(
        socket,
        *id,
        "Runtime.evaluate",
        json!({
            "expression": USER_TOKEN_EXPRESSION,
            "returnByValue": true
        }),
        Some(session_id),
    )
    .await;
    let evaluated = evaluated?;
    let token = evaluated
        .pointer("/result/result/value")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty() && HeaderValue::from_str(token).is_ok())
        .map(str::to_string);
    Ok(token)
}

async fn detach_page_session(socket: &mut CdpSocket, id: &mut u64, session_id: &str) {
    *id += 1;
    let _ = cdp_command(
        socket,
        *id,
        "Target.detachFromTarget",
        json!({"sessionId": session_id}),
        None,
    )
    .await;
}

fn service_now_cookie_header(document: &Value, host: &str, is_https: bool) -> Option<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let mut cookies = document
        .pointer("/result/cookies")?
        .as_array()?
        .iter()
        .filter_map(|cookie| {
            let name = cookie.get("name")?.as_str()?;
            let value = cookie.get("value")?.as_str()?;
            let domain = cookie.get("domain")?.as_str()?.trim_start_matches('.');
            let path = cookie.get("path").and_then(Value::as_str).unwrap_or("/");
            let secure = cookie
                .get("secure")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let expires = cookie
                .get("expires")
                .and_then(Value::as_f64)
                .unwrap_or(-1.0);
            let domain_matches = host.eq_ignore_ascii_case(domain)
                || host
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{}", domain.to_ascii_lowercase()));
            let usable = !name.is_empty()
                && !name
                    .bytes()
                    .any(|byte| matches!(byte, b';' | b'=' | b'\r' | b'\n'))
                && !value
                    .bytes()
                    .any(|byte| matches!(byte, b';' | b'\r' | b'\n'))
                && domain_matches
                && "/api/now/".starts_with(path)
                && (!secure || is_https)
                && (expires <= 0.0 || expires > now);
            usable.then_some((path.len(), name, value))
        })
        .collect::<Vec<_>>();
    cookies.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
    (!cookies.is_empty()).then(|| {
        cookies
            .into_iter()
            .map(|(_, name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    })
}

enum SessionValidation {
    Authenticated(String),
    Waiting(u16),
}

async fn validate_session(
    site_url: &str,
    cookie: &str,
    user_token: &str,
) -> Result<SessionValidation, ApiError> {
    let cookies = BrowserCookies::new(site_url, cookie)?;
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .cookie_provider(cookies.provider())
        .build()?;
    for attempt in 0..2 {
        let response = http
            .get(format!("{site_url}/api/now/table/sys_user"))
            .query(&[
                ("sysparm_query", "sys_id=javascript:gs.getUserID()"),
                ("sysparm_fields", "sys_id,user_name,name"),
                ("sysparm_limit", "1"),
            ])
            .header("X-UserToken", user_token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;
        if response.status().is_success() {
            let cookie = cookies.current_header().ok_or_else(|| {
                ApiError::Auth("browser-session validation removed every session cookie".into())
            })?;
            return Ok(SessionValidation::Authenticated(cookie));
        }
        let logged_in = response
            .headers()
            .get("x-is-logged-in")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("true"));
        let status = response.status().as_u16();
        let waiting = status == 401 || status == 403 && !logged_in || (300..=399).contains(&status);
        if attempt == 0 && waiting && cookies.refreshed_header().is_some() {
            continue;
        }
        return match status {
            status if waiting => Ok(SessionValidation::Waiting(status)),
            403 => Err(ApiError::Auth(
                "browser sign-in succeeded, but this account is not allowed to use the ServiceNow Table API"
                    .into(),
            )),
            status => Err(ApiError::Other(format!(
                "browser-session validation returned HTTP {status}"
            ))),
        };
    }
    unreachable!("browser session validation makes at most two attempts")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserAlias {
    Auto,
    Chrome,
    Edge,
    Chromium,
    WindowsChrome,
    WindowsEdge,
    WindowsChromium,
}

impl BrowserAlias {
    fn parse(value: &OsStr) -> Option<Self> {
        match value.to_str()?.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "chrome" => Some(Self::Chrome),
            "edge" => Some(Self::Edge),
            "chromium" => Some(Self::Chromium),
            "windows-chrome" => Some(Self::WindowsChrome),
            "windows-edge" => Some(Self::WindowsEdge),
            "windows-chromium" => Some(Self::WindowsChromium),
            _ => None,
        }
    }

    fn native_variant(self) -> Option<Self> {
        match self {
            Self::Auto | Self::Chrome | Self::Edge | Self::Chromium => Some(self),
            Self::WindowsChrome | Self::WindowsEdge | Self::WindowsChromium => None,
        }
    }
}

enum BrowserBackend {
    Native(PathBuf),
    WindowsBridge(Option<OsString>),
}

fn select_browser_backend() -> Result<BrowserBackend, ApiError> {
    let preference = std::env::var_os("SERVICENOW_BROWSER");
    if cfg!(target_os = "windows") {
        return Ok(BrowserBackend::WindowsBridge(preference));
    }
    if !is_wsl() {
        return find_native_browser(preference.as_deref()).map(BrowserBackend::Native);
    }
    select_wsl_browser_backend(preference)
}

fn select_wsl_browser_backend(preference: Option<OsString>) -> Result<BrowserBackend, ApiError> {
    select_wsl_browser_backend_with(preference, find_native_browser)
}

fn select_wsl_browser_backend_with(
    preference: Option<OsString>,
    find_native: impl FnOnce(Option<&OsStr>) -> Result<PathBuf, ApiError>,
) -> Result<BrowserBackend, ApiError> {
    if preference
        .as_deref()
        .is_some_and(looks_like_windows_browser_preference)
    {
        return Ok(BrowserBackend::WindowsBridge(preference));
    }
    match find_native(preference.as_deref()) {
        Ok(browser) => Ok(BrowserBackend::Native(browser)),
        Err(_error)
            if preference.is_none()
                || preference
                    .as_deref()
                    .and_then(BrowserAlias::parse)
                    .is_some() =>
        {
            Ok(BrowserBackend::WindowsBridge(preference))
        }
        Err(error) => Err(error),
    }
}

fn find_native_browser(preference: Option<&OsStr>) -> Result<PathBuf, ApiError> {
    if let Some(preference) = preference {
        if let Some(alias) = BrowserAlias::parse(preference) {
            let Some(alias) = alias.native_variant() else {
                return Err(ApiError::InvalidInput(format!(
                    "SERVICENOW_BROWSER={} selects a Windows browser and can only be used on Windows or WSL",
                    preference.to_string_lossy()
                )));
            };
            return find_native_browser_alias(alias).ok_or_else(|| {
                ApiError::InvalidInput(format!(
                    "SERVICENOW_BROWSER={} was requested, but no matching browser is installed; use chrome, edge, chromium, windows-chrome, windows-edge, or an executable path",
                    preference.to_string_lossy()
                ))
            });
        }
        return resolve_program(Path::new(preference)).ok_or_else(|| {
            ApiError::InvalidInput(format!(
                "SERVICENOW_BROWSER does not identify an executable or friendly browser name: {}",
                Path::new(preference).display()
            ))
        });
    }

    find_native_browser_alias(BrowserAlias::Auto).ok_or_else(|| {
        ApiError::InvalidInput(
            "browser sign-in needs Chrome, Edge, or Chromium; install one or set SERVICENOW_BROWSER to chrome, edge, chromium, or an executable path"
                .into(),
        )
    })
}

fn find_native_browser_alias(alias: BrowserAlias) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let absolute = [
        (
            BrowserAlias::Chrome,
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ),
        (
            BrowserAlias::Edge,
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ),
        (
            BrowserAlias::Chromium,
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ),
    ];
    #[cfg(not(target_os = "macos"))]
    let absolute: [(BrowserAlias, &str); 0] = [];
    for (kind, candidate) in absolute {
        if (matches!(alias, BrowserAlias::Auto) || alias == kind)
            && let Some(path) = resolve_program(Path::new(candidate))
        {
            return Some(path);
        }
    }

    let candidates: &[&str] = match alias {
        BrowserAlias::Auto => &[
            "google-chrome",
            "google-chrome-stable",
            "microsoft-edge",
            "microsoft-edge-stable",
            "chromium",
            "chromium-browser",
        ],
        BrowserAlias::Chrome => &["google-chrome", "google-chrome-stable", "chrome"],
        BrowserAlias::Edge => &["microsoft-edge", "microsoft-edge-stable", "msedge"],
        BrowserAlias::Chromium => &["chromium", "chromium-browser"],
        BrowserAlias::WindowsChrome | BrowserAlias::WindowsEdge | BrowserAlias::WindowsChromium => {
            return None;
        }
    };
    candidates.iter().find_map(find_in_path)
}

fn resolve_program(candidate: &Path) -> Option<PathBuf> {
    if candidate.components().count() > 1 {
        candidate.is_file().then(|| candidate.to_path_buf())
    } else {
        find_in_path(candidate.as_os_str())
    }
}

fn find_in_path(candidate: impl AsRef<OsStr>) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join(candidate.as_ref()))
            .find(|path| path.is_file())
    })
}

fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var_os("WSL_INTEROP").is_some()
            || std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .is_ok_and(|value| value.to_ascii_lowercase().contains("microsoft")))
}

fn looks_like_windows_browser_preference(preference: &OsStr) -> bool {
    if matches!(
        BrowserAlias::parse(preference),
        Some(
            BrowserAlias::WindowsChrome | BrowserAlias::WindowsEdge | BrowserAlias::WindowsChromium
        )
    ) {
        return true;
    }
    let value = preference.to_string_lossy();
    let bytes = value.as_bytes();
    value.to_ascii_lowercase().ends_with(".exe")
        || value.starts_with(r"\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
}

async fn windows_browser_cookie(
    site_url: &str,
    browser_override: Option<&OsStr>,
    progress: &mut ProgressReporter<'_>,
) -> Result<BrowserSession, ApiError> {
    let powershell = ["powershell.exe", "pwsh.exe"]
        .into_iter()
        .find_map(find_in_path)
        .ok_or_else(|| {
            ApiError::InvalidInput(
                "WSL browser sign-in needs Windows PowerShell interop; ensure powershell.exe is available on PATH"
                    .into(),
            )
        })?;
    let bridge = render_windows_bridge(site_url, browser_override);
    let mut child = tokio::process::Command::new(powershell)
        .args(POWERSHELL_ARGS)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| ApiError::Other(format!("failed to start PowerShell: {error}")))?;
    let mut stdin = child
        .stdin
        .take()
        .expect("PowerShell stdin was configured as piped");
    stdin.write_all(bridge.as_bytes()).await.map_err(|error| {
        ApiError::Other(format!(
            "failed to send browser bridge to PowerShell: {error}"
        ))
    })?;
    drop(stdin);
    let stdout = child
        .stdout
        .take()
        .expect("PowerShell stdout was configured as piped");
    let mut stderr = child
        .stderr
        .take()
        .expect("PowerShell stderr was configured as piped");
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await.map(|_| bytes)
    });
    let mut stdout = stdout;
    let mut stdout_bytes = Vec::new();
    loop {
        let mut chunk = [0_u8; 4096];
        let read = stdout
            .read(&mut chunk)
            .await
            .map_err(|error| ApiError::Other(format!("browser bridge failed: {error}")))?;
        if read == 0 {
            break;
        }
        stdout_bytes.extend_from_slice(&chunk[..read]);
        let decoded = decode_powershell_output(&stdout_bytes);
        for output_line in decoded.lines() {
            if let Some(stage) = powershell_progress(output_line) {
                progress.report(stage);
            }
        }
    }
    child
        .wait()
        .await
        .map_err(|error| ApiError::Other(format!("browser bridge failed: {error}")))?;
    let stderr = stderr_task
        .await
        .map_err(|error| ApiError::Other(format!("browser bridge failed: {error}")))?
        .map_err(|error| ApiError::Other(format!("browser bridge failed: {error}")))?;

    let stdout = decode_powershell_output(&stdout_bytes);
    if let Some(encoded) = stdout
        .lines()
        .find_map(|line| line.strip_prefix("SERVICENOW_RESULT:"))
    {
        return serde_json::from_str(encoded).map_err(|error| {
            ApiError::Other(format!("browser returned invalid session data: {error}"))
        });
    }
    if stdout.lines().any(|line| line == "SERVICENOW_FORBIDDEN") {
        return Err(ApiError::Auth(
            "browser sign-in succeeded, but this account is not allowed to use the ServiceNow Table API"
                .into(),
        ));
    }
    if let Some(detail) = powershell_bridge_error(&stdout) {
        return Err(ApiError::Other(format!("browser sign-in failed: {detail}")));
    }
    let detail = powershell_error_detail(&stderr);
    Err(ApiError::Other(if !detail.is_empty() {
        format!("browser sign-in failed: {detail}")
    } else if stdout.lines().any(|line| line == "SERVICENOW_BRIDGE_READY") {
        "browser sign-in ended before a ServiceNow session was returned; try again or set SERVICENOW_BROWSER to Windows Edge or Chrome"
            .into()
    } else {
        "Windows PowerShell exited before starting the browser sign-in bridge; ensure WSL Windows interop is enabled and try again"
            .into()
    }))
}

fn powershell_progress(line: &str) -> Option<BrowserProgress> {
    let stage = line.trim().strip_prefix("SERVICENOW_STAGE:")?;
    match stage {
        "starting-private-browser" => Some(BrowserProgress::StartingPrivateBrowser),
        "private-browser-opened" => Some(BrowserProgress::PrivateBrowserOpened),
        "waiting-browser-channel" => Some(BrowserProgress::WaitingForBrowserChannel),
        "browser-channel-ready" => Some(BrowserProgress::BrowserChannelReady),
        "waiting-servicenow-page" => Some(BrowserProgress::WaitingForServiceNowPage),
        "servicenow-page-detected" => Some(BrowserProgress::ServiceNowPageDetected),
        "reading-session-cookies" => Some(BrowserProgress::ReadingSessionCookies),
        "session-cookies-detected" => Some(BrowserProgress::SessionCookiesDetected),
        "reading-user-token" => Some(BrowserProgress::ReadingUserToken),
        "user-token-detected" => Some(BrowserProgress::UserTokenDetected),
        "validating-session" => Some(BrowserProgress::ValidatingSession),
        "session-validated" => Some(BrowserProgress::SessionValidated),
        value => value
            .strip_prefix("validation-http-")
            .and_then(|status| status.parse::<u16>().ok())
            .filter(|status| (100..=599).contains(status))
            .map(BrowserProgress::ValidationResponse),
    }
}

const POWERSHELL_STDIN_BOOTSTRAP: &str =
    "$script = [Console]::In.ReadToEnd(); & ([ScriptBlock]::Create($script))";
const POWERSHELL_ARGS: [&str; 7] = [
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-Command",
    POWERSHELL_STDIN_BOOTSTRAP,
];

fn decode_powershell_output(bytes: &[u8]) -> String {
    let has_utf16_le_bom = bytes.starts_with(&[0xff, 0xfe]);
    let looks_like_utf16_le = bytes.len() >= 4
        && bytes
            .iter()
            .skip(1)
            .step_by(2)
            .filter(|byte| **byte == 0)
            .count()
            * 4
            >= bytes.len();
    if has_utf16_le_bom || looks_like_utf16_le {
        let start = usize::from(has_utf16_le_bom) * 2;
        let (pairs, _) = bytes[start..].as_chunks::<2>();
        let units = pairs
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn bounded_error_detail(detail: &str) -> String {
    detail.trim().chars().take(500).collect()
}

fn powershell_bridge_error(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("SERVICENOW_ERROR:"))
        .map(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .map(|detail| bounded_error_detail(&detail))
                .filter(|detail| !detail.is_empty())
                .unwrap_or_else(|| "the Windows browser bridge returned an unreadable error".into())
        })
}

fn powershell_error_detail(bytes: &[u8]) -> String {
    let output = decode_powershell_output(bytes);
    if !output.trim_start().starts_with("#< CLIXML") {
        return bounded_error_detail(&output);
    }

    let mut errors = Vec::new();
    let mut remaining = output.as_str();
    const ERROR_NODE: &str = "<S S=\"Error\"";
    while let Some(start) = remaining.find(ERROR_NODE) {
        remaining = &remaining[start + ERROR_NODE.len()..];
        let Some(content_start) = remaining.find('>') else {
            break;
        };
        remaining = &remaining[content_start + 1..];
        let Some(content_end) = remaining.find("</S>") else {
            break;
        };
        let error = decode_clixml_text(&remaining[..content_end]);
        if !error.trim().is_empty() {
            errors.push(error);
        }
        remaining = &remaining[content_end + 4..];
    }
    bounded_error_detail(&errors.join("\n"))
}

fn decode_clixml_text(value: &str) -> String {
    let characters = value.chars().collect::<Vec<_>>();
    let mut decoded = String::with_capacity(value.len());
    let mut index = 0;
    while index < characters.len() {
        if index + 6 < characters.len()
            && characters[index] == '_'
            && characters[index + 1] == 'x'
            && characters[index + 6] == '_'
        {
            let hex = characters[index + 2..index + 6].iter().collect::<String>();
            if let Ok(codepoint) = u32::from_str_radix(&hex, 16)
                && let Some(character) = char::from_u32(codepoint)
            {
                decoded.push(character);
                index += 7;
                continue;
            }
        }
        decoded.push(characters[index]);
        index += 1;
    }
    decoded
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn powershell_hex(value: &[u8]) -> String {
    use std::fmt::Write as _;

    value.iter().fold(
        String::with_capacity(value.len() * 2),
        |mut encoded, byte| {
            write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
            encoded
        },
    )
}

fn render_windows_bridge(site_url: &str, browser: Option<&std::ffi::OsStr>) -> String {
    WINDOWS_BROWSER_BRIDGE
        .replacen("__INSTANCE_HEX__", &powershell_hex(site_url.as_bytes()), 1)
        .replacen(
            "__BROWSER_HEX__",
            &browser
                .map(|value| powershell_hex(value.to_string_lossy().as_bytes()))
                .unwrap_or_default(),
            1,
        )
        .replacen(
            "__USER_TOKEN_EXPRESSION_HEX__",
            &powershell_hex(USER_TOKEN_EXPRESSION.as_bytes()),
            1,
        )
}

const WINDOWS_BROWSER_BRIDGE: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$OutputEncoding = [Console]::OutputEncoding
Write-Output 'SERVICENOW_BRIDGE_READY'
$script:reportedStages = @{}
function Write-Stage {
    param([string]$Stage)
    if (-not $script:reportedStages.ContainsKey($Stage)) {
        $script:reportedStages[$Stage] = $true
        Write-Output ('SERVICENOW_STAGE:' + $Stage)
    }
}
function ConvertFrom-HexUtf8 {
    param([string]$Hex)
    if (-not $Hex) { return '' }
    $bytes = New-Object byte[] ($Hex.Length / 2)
    for ($index = 0; $index -lt $bytes.Length; $index++) {
        $bytes[$index] = [Convert]::ToByte($Hex.Substring($index * 2, 2), 16)
    }
    return [Text.Encoding]::UTF8.GetString($bytes)
}
$instance = [Uri](ConvertFrom-HexUtf8 '__INSTANCE_HEX__')
$origin = $instance.GetLeftPart([UriPartial]::Authority)
$loginUrl = "$origin/nav_to.do?uri=incident_list.do"
$apiUrl = "$origin/api/now/table/sys_user?sysparm_query=sys_id%3Djavascript%3Ags.getUserID()&sysparm_fields=sys_id%2Cuser_name%2Cname&sysparm_limit=1"
$userTokenExpression = ConvertFrom-HexUtf8 '__USER_TOKEN_EXPRESSION_HEX__'
$browserPreference = ConvertFrom-HexUtf8 '__BROWSER_HEX__'
if (-not $browserPreference) { $browserPreference = $env:SERVICENOW_BROWSER }
$browser = $null
$browserKind = $null
$browserName = $null
if ($browserPreference -and (Test-Path -LiteralPath $browserPreference)) {
    $browser = $browserPreference
    if ($browser -match '(?i)edge') {
        $browserKind = 'edge'
        $browserName = 'Microsoft Edge'
    } elseif ($browser -match '(?i)chromium') {
        $browserKind = 'chromium'
        $browserName = 'Chromium'
    } else {
        $browserKind = 'chrome'
        $browserName = 'Google Chrome'
    }
}
if (-not $browser) {
    $requestedBrowser = if ($browserPreference) { $browserPreference.Trim().ToLowerInvariant() } else { 'auto' }
    switch -Regex ($requestedBrowser) {
        '^(auto)?$' { $candidateKinds = @('edge', 'chrome', 'chromium'); break }
        '^(edge|msedge|microsoft-edge|windows-edge)$' { $candidateKinds = @('edge'); break }
        '^(chrome|google-chrome|windows-chrome)$' { $candidateKinds = @('chrome'); break }
        '^(chromium|windows-chromium)$' { $candidateKinds = @('chromium'); break }
        default { throw "SERVICENOW_BROWSER '$browserPreference' is not a Windows browser path or friendly name. Use chrome, edge, chromium, windows-chrome, or windows-edge." }
    }
    $candidates = foreach ($kind in $candidateKinds) {
        if ($kind -eq 'edge') {
            [PSCustomObject]@{ Path = "$env:PROGRAMFILES\Microsoft\Edge\Application\msedge.exe"; Kind = 'edge'; Name = 'Microsoft Edge' }
            [PSCustomObject]@{ Path = "${env:PROGRAMFILES(X86)}\Microsoft\Edge\Application\msedge.exe"; Kind = 'edge'; Name = 'Microsoft Edge' }
            [PSCustomObject]@{ Path = "$env:LOCALAPPDATA\Microsoft\Edge\Application\msedge.exe"; Kind = 'edge'; Name = 'Microsoft Edge' }
        } elseif ($kind -eq 'chrome') {
            [PSCustomObject]@{ Path = "$env:PROGRAMFILES\Google\Chrome\Application\chrome.exe"; Kind = 'chrome'; Name = 'Google Chrome' }
            [PSCustomObject]@{ Path = "${env:PROGRAMFILES(X86)}\Google\Chrome\Application\chrome.exe"; Kind = 'chrome'; Name = 'Google Chrome' }
            [PSCustomObject]@{ Path = "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe"; Kind = 'chrome'; Name = 'Google Chrome' }
        } else {
            [PSCustomObject]@{ Path = "$env:PROGRAMFILES\Chromium\Application\chrome.exe"; Kind = 'chromium'; Name = 'Chromium' }
            [PSCustomObject]@{ Path = "${env:PROGRAMFILES(X86)}\Chromium\Application\chrome.exe"; Kind = 'chromium'; Name = 'Chromium' }
            [PSCustomObject]@{ Path = "$env:LOCALAPPDATA\Chromium\Application\chrome.exe"; Kind = 'chromium'; Name = 'Chromium' }
        }
    }
    $selection = $candidates | Where-Object { $_.Path -and (Test-Path -LiteralPath $_.Path) } | Select-Object -First 1
    if ($selection) {
        $browser = $selection.Path
        $browserKind = $selection.Kind
        $browserName = $selection.Name
    }
}
if (-not $browser) { throw 'Chrome, Edge, or Chromium was not found on Windows. Set SERVICENOW_BROWSER to chrome, edge, chromium, or a Windows executable path.' }

$policySubkey = if ($browserKind -eq 'edge') {
    'SOFTWARE\Policies\Microsoft\Edge'
} elseif ($browserKind -eq 'chromium') {
    'SOFTWARE\Policies\Chromium'
} else {
    'SOFTWARE\Policies\Google\Chrome'
}
$remoteDebuggingBlocked = $false
foreach ($registryRoot in @('HKLM:', 'HKCU:')) {
    $policy = Get-ItemProperty -LiteralPath ($registryRoot + '\' + $policySubkey) -Name 'RemoteDebuggingAllowed' -ErrorAction SilentlyContinue
    if ($policy -and $policy.PSObject.Properties['RemoteDebuggingAllowed'] -and [int]$policy.RemoteDebuggingAllowed -eq 0) {
        $remoteDebuggingBlocked = $true
        break
    }
}
if ($remoteDebuggingBlocked) {
    throw "$browserName browser sign-in is blocked by the managed RemoteDebuggingAllowed policy. Install Chrome, Edge, or Chromium inside WSL, or set SERVICENOW_BROWSER to another installed browser."
}

$profile = Join-Path $env:TEMP ("servicenow-cli-browser-" + [Guid]::NewGuid().ToString('N'))
$process = $null
$socket = $null
$bridgeExitCode = 0
$requestId = 0
function Invoke-CdpCommand {
    param($Socket, [ref]$RequestId, [string]$Method, $Params, [string]$SessionId)
    $RequestId.Value++
    $requestObject = @{ id = $RequestId.Value; method = $Method; params = $Params }
    if ($SessionId) { $requestObject.sessionId = $SessionId }
    $request = $requestObject | ConvertTo-Json -Compress -Depth 6
    $requestBytes = [Text.Encoding]::UTF8.GetBytes($request)
    $Socket.SendAsync([ArraySegment[byte]]::new($requestBytes), [Net.WebSockets.WebSocketMessageType]::Text, $true, [Threading.CancellationToken]::None).GetAwaiter().GetResult()
    while ($true) {
        $stream = [IO.MemoryStream]::new()
        do {
            $buffer = New-Object byte[] 65536
            $result = $Socket.ReceiveAsync([ArraySegment[byte]]::new($buffer), [Threading.CancellationToken]::None).GetAwaiter().GetResult()
            if ($result.MessageType -eq [Net.WebSockets.WebSocketMessageType]::Close) { throw 'The private browser sign-in channel closed.' }
            $stream.Write($buffer, 0, $result.Count)
        } while (-not $result.EndOfMessage)
        $candidate = [Text.Encoding]::UTF8.GetString($stream.ToArray()) | ConvertFrom-Json
        if ($candidate.id -eq $RequestId.Value) {
            if ($candidate.error) { throw "Browser rejected ${Method}: $($candidate.error.message)" }
            return $candidate
        }
    }
}
try {
    $portProbe = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    try {
        $portProbe.Start()
        $debugPort = ([Net.IPEndPoint]$portProbe.LocalEndpoint).Port
    } finally {
        $portProbe.Stop()
    }
    $quotedProfile = '"' + $profile + '"'
    $quotedUrl = '"' + $loginUrl + '"'
    $privateMode = if ($browserKind -eq 'edge') { '--inprivate' } else { '--incognito' }
    $arguments = "--remote-debugging-port=$debugPort --remote-debugging-address=127.0.0.1 --user-data-dir=$quotedProfile --no-first-run --no-default-browser-check --disable-sync $privateMode --new-window $quotedUrl"
    Write-Stage 'starting-private-browser'
    $process = Start-Process -FilePath $browser -ArgumentList $arguments -PassThru
    Write-Stage 'private-browser-opened'
    Write-Stage 'waiting-browser-channel'
    $channelDeadline = [DateTime]::UtcNow.AddSeconds(20)
    $version = $null
    while (-not $version -and [DateTime]::UtcNow -lt $channelDeadline) {
        if ($process.HasExited) { throw "Browser closed before sign-in completed ($($process.ExitCode))." }
        try {
            $version = Invoke-RestMethod -UseBasicParsing "http://127.0.0.1:$debugPort/json/version" -TimeoutSec 2
        } catch {}
        if (-not $version) { Start-Sleep -Milliseconds 200 }
    }
    if (-not $version.webSocketDebuggerUrl) {
        throw 'The private browser opened, but its localhost sign-in channel did not become available within 20 seconds. Edge or Chrome may have ignored its isolated debugging options, or an enterprise browser policy may block browser automation. Set SERVICENOW_BROWSER to the other Windows browser and retry with --verbose.'
    }

    Write-Stage 'browser-channel-ready'
    $socket = [Net.WebSockets.ClientWebSocket]::new()
    $socket.ConnectAsync([Uri]$version.webSocketDebuggerUrl, [Threading.CancellationToken]::None).GetAwaiter().GetResult()
    $deadline = [DateTime]::UtcNow.AddMinutes(5)
    $lastStage = 'the authenticated ServiceNow page'
    Write-Stage 'waiting-servicenow-page'
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($process.HasExited) { throw "Browser closed before sign-in completed ($($process.ExitCode))." }
        $targets = Invoke-CdpCommand $socket ([ref]$requestId) 'Target.getTargets' @{} $null
        $target = $targets.result.targetInfos | Where-Object {
            $_.type -eq 'page' -and ($_.url -eq $origin -or $_.url.StartsWith($origin + '/'))
        } | Select-Object -First 1
        if (-not $target) {
            Start-Sleep -Milliseconds 750
            continue
        }
        Write-Stage 'servicenow-page-detected'
        $lastStage = 'ServiceNow session cookies'
        Write-Stage 'reading-session-cookies'
        $attached = Invoke-CdpCommand $socket ([ref]$requestId) 'Target.attachToTarget' @{ targetId = $target.targetId; flatten = $true } $null
        $sessionId = $attached.result.sessionId
        try {
            $document = Invoke-CdpCommand $socket ([ref]$requestId) 'Network.getCookies' @{ urls = @("$origin/api/now/") } $sessionId
            $now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
            $cookies = @($document.result.cookies | Where-Object {
                $nameIsSafe = $_.name -and $_.name -notmatch '[;=\r\n]'
                $valueIsSafe = $_.value -notmatch '[;\r\n]'
                $domain = $_.domain.TrimStart('.').ToLowerInvariant()
                $hostMatches = $instance.Host.ToLowerInvariant() -eq $domain -or $instance.Host.ToLowerInvariant().EndsWith('.' + $domain)
                $cookiePath = if ($_.path) { $_.path } else { '/' }
                $pathMatches = '/api/now/'.StartsWith($cookiePath)
                $notExpired = $_.expires -le 0 -or $_.expires -gt $now
                $nameIsSafe -and $valueIsSafe -and $hostMatches -and $pathMatches -and $notExpired -and (-not $_.secure -or $instance.Scheme -eq 'https')
            } | Sort-Object { $_.path.Length } -Descending)
            if ($cookies.Count -gt 0) {
                Write-Stage 'session-cookies-detected'
                $cookieHeader = ($cookies | ForEach-Object { "$($_.name)=$($_.value)" }) -join '; '
                $webSession = [Microsoft.PowerShell.Commands.WebRequestSession]::new()
                foreach ($sourceCookie in $cookies) {
                    $cookiePath = if ($sourceCookie.path) { $sourceCookie.path } else { '/' }
                    $sessionCookie = [Net.Cookie]::new($sourceCookie.name, $sourceCookie.value, $cookiePath, $instance.Host)
                    $webSession.Cookies.Add($instance, $sessionCookie)
                }
                $lastStage = 'the ServiceNow user token (g_ck or sysparm_ck)'
                Write-Stage 'reading-user-token'
                $evaluated = Invoke-CdpCommand $socket ([ref]$requestId) 'Runtime.evaluate' @{ expression = $userTokenExpression; returnByValue = $true } $sessionId
                $userToken = $evaluated.result.result.value
                if ($userToken -and $userToken -notmatch '[\r\n]') {
                    Write-Stage 'user-token-detected'
                    $lastStage = 'ServiceNow REST session validation'
                    Write-Stage 'validating-session'
                    try {
                        $response = Invoke-WebRequest -UseBasicParsing -Uri $apiUrl -WebSession $webSession -Headers @{ 'X-UserToken' = $userToken; Accept = 'application/json' } -MaximumRedirection 0 -TimeoutSec 10
                        if ($response.StatusCode -ge 200 -and $response.StatusCode -lt 300) {
                            $cookieHeader = $webSession.Cookies.GetCookieHeader([Uri]$apiUrl)
                            if (-not $cookieHeader) { throw 'ServiceNow validation removed every session cookie.' }
                            Write-Stage 'session-validated'
                            $session = @{ cookie = $cookieHeader; user_token = $userToken }
                            Write-Output ('SERVICENOW_RESULT:' + ($session | ConvertTo-Json -Compress))
                            exit 0
                        }
                    } catch {
                        $status = 0
                        $loggedIn = $false
                        if ($_.Exception.Response) {
                            $status = [int]$_.Exception.Response.StatusCode
                            $loggedIn = $_.Exception.Response.Headers['X-Is-Logged-In'] -eq 'true'
                            $lastStage = "ServiceNow REST session validation (HTTP $status)"
                            Write-Stage ("validation-http-" + $status)
                        }
                        if ($status -eq 403 -and $loggedIn) {
                            Write-Output 'SERVICENOW_FORBIDDEN'
                            exit 3
                        }
                        if ($status -ne 401 -and $status -ne 403 -and ($status -lt 300 -or $status -ge 400)) { throw }
                    }
                }
            }
        } finally {
            if ($sessionId) { try { Invoke-CdpCommand $socket ([ref]$requestId) 'Target.detachFromTarget' @{ sessionId = $sessionId } $null | Out-Null } catch {} }
        }
        Start-Sleep -Milliseconds 750
    }
    throw "Browser sign-in timed out after five minutes while waiting for $lastStage; no credential was stored."
} catch {
    $message = $_.Exception.Message
    if (-not $message) { $message = [string]$_ }
    $errorBytes = [Text.Encoding]::UTF8.GetBytes($message)
    Write-Output ('SERVICENOW_ERROR:' + [Convert]::ToBase64String($errorBytes))
    $bridgeExitCode = 1
} finally {
    if ($socket) {
        try { Invoke-CdpCommand $socket ([ref]$requestId) 'Browser.close' @{} $null | Out-Null } catch {}
        try { $socket.Dispose() } catch {}
    }
    if ($process -and -not $process.HasExited) { try { Stop-Process -Id $process.Id -Force } catch {} }
    if (Test-Path -LiteralPath $profile) { try { Remove-Item -LiteralPath $profile -Recurse -Force } catch {} }
}
if ($bridgeExitCode -ne 0) { exit $bridgeExitCode }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::accept_async;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn cookie_header_is_scoped_to_the_instance_and_api_path() {
        let document = json!({
            "result": {"cookies": [
                {"name": "JSESSIONID", "value": "instance-session", "domain": ".service-now.com", "path": "/", "secure": true, "expires": -1},
                {"name": "route", "value": "api-route", "domain": "company.service-now.com", "path": "/api", "secure": true, "expires": -1},
                {"name": "entra", "value": "must-not-leak", "domain": ".microsoftonline.com", "path": "/", "secure": true, "expires": -1},
                {"name": "ui-only", "value": "skip", "domain": "company.service-now.com", "path": "/now", "secure": true, "expires": -1},
                {"name": "single-endpoint", "value": "skip", "domain": "company.service-now.com", "path": "/api/now/table/sys_user", "secure": true, "expires": -1}
            ]}
        });
        assert_eq!(
            service_now_cookie_header(&document, "company.service-now.com", true).as_deref(),
            Some("route=api-route; JSESSIONID=instance-session")
        );
    }

    #[test]
    fn invalid_cookie_characters_are_never_forwarded() {
        let document = json!({
            "result": {"cookies": [
                {"name": "safe", "value": "value", "domain": "company.service-now.com", "path": "/", "secure": true, "expires": -1},
                {"name": "unsafe", "value": "value\r\nX-Evil: yes", "domain": "company.service-now.com", "path": "/", "secure": true, "expires": -1}
            ]}
        });
        assert_eq!(
            service_now_cookie_header(&document, "company.service-now.com", true).as_deref(),
            Some("safe=value")
        );
    }

    #[test]
    fn powershell_bridge_uses_an_isolated_profile_and_service_now_scope() {
        assert!(WINDOWS_BROWSER_BRIDGE.contains("--user-data-dir="));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("--remote-debugging-port=$debugPort"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("[Net.IPAddress]::Loopback, 0"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("AddSeconds(20)"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("AddMinutes(5)"));
        assert!(!WINDOWS_BROWSER_BRIDGE.contains("DevToolsActivePort"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("--inprivate"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("--incognito"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("$ProgressPreference = 'SilentlyContinue'"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("SERVICENOW_STAGE:"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("SERVICENOW_ERROR:"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("Network.getCookies"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("'Browser.close'"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("'X-UserToken' = $userToken"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("-WebSession $webSession"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("GetCookieHeader([Uri]$apiUrl)"));
        assert!(!WINDOWS_BROWSER_BRIDGE.contains("@{ Cookie = $cookieHeader"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("$instance.Host"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("__INSTANCE_HEX__"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("__USER_TOKEN_EXPRESSION_HEX__"));
        assert!(!WINDOWS_BROWSER_BRIDGE.contains("login.microsoftonline.com"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("SOFTWARE\\Policies\\Microsoft\\Edge"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("SOFTWARE\\Policies\\Google\\Chrome"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("RemoteDebuggingAllowed"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("managed RemoteDebuggingAllowed policy"));
        let policy_check = WINDOWS_BROWSER_BRIDGE
            .find("$remoteDebuggingBlocked")
            .unwrap();
        let browser_start = WINDOWS_BROWSER_BRIDGE.find("Start-Process").unwrap();
        assert!(policy_check < browser_start);
        assert_eq!(
            powershell_hex(b"https://company.service-now.com"),
            "68747470733a2f2f636f6d70616e792e736572766963652d6e6f772e636f6d"
        );
        let rendered = render_windows_bridge("https://company.service-now.com", None);
        assert!(!rendered.contains("__INSTANCE_HEX__"));
        assert!(!rendered.contains("__BROWSER_HEX__"));
        assert!(!rendered.contains("__USER_TOKEN_EXPRESSION_HEX__"));
    }

    #[test]
    fn private_browsing_flag_matches_the_browser() {
        assert_eq!(
            private_browsing_argument(Path::new("/usr/bin/google-chrome")),
            "--incognito"
        );
        assert_eq!(
            private_browsing_argument(Path::new("/usr/bin/chromium")),
            "--incognito"
        );
        assert_eq!(
            private_browsing_argument(Path::new(
                r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"
            )),
            "--inprivate"
        );
    }

    #[test]
    fn friendly_browser_names_are_case_insensitive_and_platform_aware() {
        assert_eq!(
            BrowserAlias::parse(OsStr::new("chrome")),
            Some(BrowserAlias::Chrome)
        );
        assert_eq!(
            BrowserAlias::parse(OsStr::new("EDGE")),
            Some(BrowserAlias::Edge)
        );
        assert_eq!(
            BrowserAlias::parse(OsStr::new(" Chromium ")),
            Some(BrowserAlias::Chromium)
        );
        assert_eq!(
            BrowserAlias::parse(OsStr::new("windows-edge")),
            Some(BrowserAlias::WindowsEdge)
        );
        assert_eq!(BrowserAlias::parse(OsStr::new("firefox")), None);
        assert!(looks_like_windows_browser_preference(OsStr::new(
            "windows-chrome"
        )));
        assert!(looks_like_windows_browser_preference(OsStr::new(
            r"C:\Program Files\Google\Chrome\Application\chrome.exe"
        )));
        assert!(!looks_like_windows_browser_preference(OsStr::new(
            "/usr/bin/google-chrome"
        )));
    }

    #[test]
    fn wsl_prefers_a_native_linux_browser_and_falls_back_to_windows() {
        let native =
            select_wsl_browser_backend_with(None, |_| Ok(PathBuf::from("/usr/bin/chromium")))
                .unwrap();
        assert!(
            matches!(native, BrowserBackend::Native(path) if path == Path::new("/usr/bin/chromium"))
        );

        let fallback = select_wsl_browser_backend_with(Some(OsString::from("chrome")), |_| {
            Err(ApiError::InvalidInput("not installed in WSL".into()))
        })
        .unwrap();
        assert!(matches!(
            fallback,
            BrowserBackend::WindowsBridge(Some(value)) if value == OsStr::new("chrome")
        ));

        let forced_windows =
            select_wsl_browser_backend_with(Some(OsString::from("windows-edge")), |_| {
                panic!("a forced Windows alias must not probe Linux browsers")
            })
            .unwrap();
        assert!(matches!(
            forced_windows,
            BrowserBackend::WindowsBridge(Some(value)) if value == OsStr::new("windows-edge")
        ));
    }

    #[test]
    fn powershell_bridge_is_sent_over_stdin_instead_of_the_command_line() {
        assert_eq!(
            POWERSHELL_STDIN_BOOTSTRAP,
            "$script = [Console]::In.ReadToEnd(); & ([ScriptBlock]::Create($script))"
        );
        assert!(!POWERSHELL_STDIN_BOOTSTRAP.contains("WINDOWS_BROWSER_BRIDGE"));
        assert!(!POWERSHELL_STDIN_BOOTSTRAP.contains("EncodedCommand"));
        assert!(POWERSHELL_ARGS.iter().all(|argument| argument.len() < 256));
        let bridge = render_windows_bridge("https://company.service-now.com", None);
        assert!(
            POWERSHELL_ARGS
                .iter()
                .all(|argument| !argument.contains(&bridge))
        );
    }

    #[test]
    fn powershell_output_accepts_utf8_and_utf16_le() {
        let output = "SERVICENOW_RESULT:{\"cookie\":\"safe\"}\r\n";
        let utf16 = output
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(decode_powershell_output(output.as_bytes()), output);
        assert_eq!(decode_powershell_output(&utf16), output);
    }

    #[test]
    fn powershell_progress_accepts_only_allowlisted_secret_free_stages() {
        assert_eq!(
            powershell_progress("SERVICENOW_STAGE:waiting-servicenow-page"),
            Some(BrowserProgress::WaitingForServiceNowPage)
        );
        assert_eq!(
            powershell_progress("SERVICENOW_STAGE:validation-http-401"),
            Some(BrowserProgress::ValidationResponse(401))
        );
        assert_eq!(
            powershell_progress("SERVICENOW_STAGE:validation-http-999"),
            None
        );
        assert_eq!(
            powershell_progress("SERVICENOW_STAGE:user-token-secret-value"),
            None
        );
        assert_eq!(
            powershell_progress(
                "SERVICENOW_RESULT:{\"cookie\":\"JSESSIONID=secret\",\"user_token\":\"secret\"}"
            ),
            None
        );
    }

    #[test]
    fn progress_reporter_emits_each_milestone_once() {
        let mut events = Vec::new();
        let mut callback = |progress| events.push(progress);
        let mut reporter = ProgressReporter {
            callback: &mut callback,
            reported: HashSet::new(),
        };
        reporter.report(BrowserProgress::ReadingUserToken);
        reporter.report(BrowserProgress::ReadingUserToken);
        reporter.report(BrowserProgress::ValidationResponse(401));
        reporter.report(BrowserProgress::ValidationResponse(401));
        drop(reporter);
        assert_eq!(
            events,
            vec![
                BrowserProgress::ReadingUserToken,
                BrowserProgress::ValidationResponse(401)
            ]
        );
    }

    #[test]
    fn powershell_progress_clixml_is_not_presented_as_an_error() {
        let progress = br#"#< CLIXML
<Objs Version="1.1.0.1" xmlns="http://schemas.microsoft.com/powershell/2004/04"><Obj S="progress"><MS><PR N="Record"><AV>Preparing modules for first use.</AV></PR></MS></Obj></Objs>"#;
        assert_eq!(powershell_error_detail(progress), "");
    }

    #[test]
    fn powershell_bridge_error_protocol_preserves_plain_text() {
        use base64::engine::general_purpose::STANDARD;

        let encoded =
            STANDARD.encode("Browser sign-in timed out while waiting for the user token.");
        assert_eq!(
            powershell_bridge_error(&format!(
                "SERVICENOW_BRIDGE_READY\r\nSERVICENOW_ERROR:{encoded}\r\n"
            ))
            .as_deref(),
            Some("Browser sign-in timed out while waiting for the user token.")
        );
    }

    #[test]
    fn powershell_clixml_errors_are_rendered_as_plain_text() {
        let error = br#"#< CLIXML
<Objs Version="1.1.0.1" xmlns="http://schemas.microsoft.com/powershell/2004/04"><S S="Error">Browser rejected &quot;Target.getTargets&quot;._x000D__x000A_</S></Objs>"#;
        assert_eq!(
            powershell_error_detail(error),
            "Browser rejected \"Target.getTargets\"."
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn stdin_powershell_command_executes_and_returns_output() {
        use std::io::Write as _;

        let powershell = find_in_path("powershell.exe").expect("powershell.exe on Windows");
        let mut child = std::process::Command::new(powershell)
            .args(POWERSHELL_ARGS)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"Write-Output 'SERVICENOW_BRIDGE_READY'")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            decode_powershell_output(&output.stderr)
        );
        assert_eq!(
            decode_powershell_output(&output.stdout).trim(),
            "SERVICENOW_BRIDGE_READY"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn powershell_bridge_has_valid_windows_powershell_syntax() {
        use std::io::Write as _;

        let powershell = find_in_path("powershell.exe").expect("powershell.exe on Windows");
        let parser = r#"$tokens=$null; $errors=$null; [void][System.Management.Automation.Language.Parser]::ParseInput([Console]::In.ReadToEnd(), [ref]$tokens, [ref]$errors); if ($errors.Count) { $errors | ForEach-Object { [Console]::Error.WriteLine($_.Message) }; exit 1 }"#;
        let mut child = std::process::Command::new(powershell)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                parser,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(render_windows_bridge("https://company.service-now.com", None).as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn cdp_session_is_validated_and_rotation_is_captured_before_returning() {
        let instance = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/sys_user"))
            .and(header("cookie", "JSESSIONID=validated-session"))
            .and(header("x-usertoken", "synthetic-user-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(
                        "set-cookie",
                        "JSESSIONID=rotated-validation-session; Path=/; HttpOnly",
                    )
                    .set_body_json(json!({
                        "result": [{"sys_id": "0123456789abcdef0123456789abcdef"}]
                    })),
            )
            .expect(1)
            .mount(&instance)
            .await;

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let instance_port = instance.address().port();
        let cdp = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(stream).await.unwrap();
            while let Some(request) = socket.next().await {
                let request = request.unwrap();
                let request: Value = serde_json::from_str(request.to_text().unwrap()).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "Network.getCookies" => {
                        assert_eq!(request["sessionId"], "attached-page");
                        assert_eq!(
                            request["params"]["urls"][0],
                            format!("http://127.0.0.1:{instance_port}/api/now/")
                        );
                        json!({"cookies": [{
                            "name": "JSESSIONID",
                            "value": "validated-session",
                            "domain": "127.0.0.1",
                            "path": "/",
                            "secure": false,
                            "expires": -1
                        }]})
                    }
                    "Target.getTargets" => json!({"targetInfos": [{
                        "type": "page",
                        "url": format!("http://127.0.0.1:{instance_port}/now/nav/ui"),
                        "targetId": "service-now-page"
                    }]}),
                    "Target.attachToTarget" => json!({"sessionId": "attached-page"}),
                    "Runtime.evaluate" => {
                        assert_eq!(request["params"]["expression"], USER_TOKEN_EXPRESSION);
                        json!({"result": {
                            "type": "string",
                            "value": "synthetic-user-token"
                        }})
                    }
                    "Target.detachFromTarget" => json!({}),
                    method => panic!("unexpected CDP method: {method}"),
                };
                socket
                    .send(Message::Text(
                        json!({"id": request["id"], "result": result})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                if request["method"] == "Target.detachFromTarget" {
                    break;
                }
            }
        });

        let mut events = Vec::new();
        let mut callback = |progress| events.push(progress);
        let mut progress = ProgressReporter {
            callback: &mut callback,
            reported: HashSet::new(),
        };
        let session = wait_for_session_cookie_with_progress(
            &format!("ws://{address}"),
            &instance.uri(),
            None,
            &mut progress,
        )
        .await
        .unwrap();
        drop(progress);
        assert_eq!(session.cookie, "JSESSIONID=rotated-validation-session");
        assert_eq!(session.user_token, "synthetic-user-token");
        assert_eq!(
            events,
            vec![
                BrowserProgress::WaitingForServiceNowPage,
                BrowserProgress::ServiceNowPageDetected,
                BrowserProgress::ReadingSessionCookies,
                BrowserProgress::SessionCookiesDetected,
                BrowserProgress::ReadingUserToken,
                BrowserProgress::UserTokenDetected,
                BrowserProgress::ValidatingSession,
                BrowserProgress::SessionValidated,
            ]
        );
        cdp.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "opens an installed Chromium browser"]
    async fn installed_browser_completes_a_local_session_handoff() {
        if find_native_browser(std::env::var_os("SERVICENOW_BROWSER").as_deref()).is_err() {
            return;
        }
        let instance = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nav_to.do"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(
                        "<iframe src='/classic.do'></iframe>Signed in",
                        "text/html; charset=utf-8",
                    )
                    .insert_header("set-cookie", "JSESSIONID=browser-smoke; Path=/; HttpOnly"),
            )
            .expect(1)
            .mount(&instance)
            .await;
        Mock::given(method("GET"))
            .and(path("/classic.do"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "<input type='hidden' id='sysparm_ck' value='browser-smoke-user-token'>",
                "text/html; charset=utf-8",
            ))
            .expect(1)
            .mount(&instance)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/sys_user"))
            .and(header("cookie", "JSESSIONID=browser-smoke"))
            .and(header("x-usertoken", "browser-smoke-user-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{"sys_id": "0123456789abcdef0123456789abcdef"}]
            })))
            .expect(1..)
            .mount(&instance)
            .await;

        let session = native_browser_cookie(&instance.uri()).await.unwrap();
        assert_eq!(session.cookie, "JSESSIONID=browser-smoke");
        assert_eq!(session.user_token, "browser-smoke-user-token");
    }
}
