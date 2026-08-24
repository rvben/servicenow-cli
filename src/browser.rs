use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::HeaderValue;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::api::{ApiError, normalize_instance};
use crate::credentials::StoredCredential;

const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const BROWSER_START_TIMEOUT: Duration = Duration::from_secs(20);
const SESSION_CHECK_INTERVAL: Duration = Duration::from_millis(750);

/// Sign in through an isolated Chromium profile and retain only cookies scoped
/// to the requested ServiceNow instance.
pub async fn browser_login(
    instance: &str,
    open_browser: bool,
) -> Result<StoredCredential, ApiError> {
    if !open_browser {
        return Err(ApiError::InvalidInput(
            "browser-session sign-in must open a browser; remove --no-browser or choose another --method"
                .into(),
        ));
    }
    let site_url = normalize_instance(instance)?;

    let session = if uses_windows_browser_bridge() {
        windows_browser_cookie(&site_url).await?
    } else {
        native_browser_cookie(&site_url).await?
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

async fn native_browser_cookie(site_url: &str) -> Result<BrowserSession, ApiError> {
    let browser = find_native_browser()?;
    let private_mode = private_browsing_argument(&browser);
    let profile = tempfile::tempdir()
        .map_err(|error| ApiError::Other(format!("failed to create browser profile: {error}")))?;
    let login_url = format!("{site_url}/nav_to.do?uri=incident_list.do");
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
    let mut process = NativeBrowser {
        child,
        _profile: profile,
    };
    let websocket_url = wait_for_debugger(&mut process).await?;
    wait_for_session_cookie(&websocket_url, site_url, Some(&mut process)).await
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

async fn wait_for_session_cookie(
    websocket_url: &str,
    site_url: &str,
    mut process: Option<&mut NativeBrowser>,
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
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()?;
    let started = Instant::now();
    let mut id = 0_u64;

    loop {
        if let Some(process) = process.as_deref_mut() {
            process.ensure_running()?;
        }
        id += 1;
        let targets = cdp_command(&mut socket, id, "Target.getTargets", json!({}), None).await?;
        let Some(target) = service_now_page_target(&targets, host) else {
            if started.elapsed() >= LOGIN_TIMEOUT {
                return Err(ApiError::Auth(
                    "browser sign-in timed out after five minutes; run `servicenow auth login --method browser` to try again"
                        .into(),
                ));
            }
            tokio::time::sleep(SESSION_CHECK_INTERVAL).await;
            continue;
        };
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
            if let Some(cookie) = service_now_cookie_header(&response, host, is_https)
                && let Some(user_token) =
                    service_now_user_token(&mut socket, &mut id, &session_id).await?
            {
                match validate_session(&http, site_url, &cookie, &user_token).await? {
                    SessionValidation::Authenticated => {
                        return Ok(Some(BrowserSession { cookie, user_token }));
                    }
                    SessionValidation::Waiting => {}
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
            return Err(ApiError::Auth(
                "browser sign-in timed out after five minutes; run `servicenow auth login --method browser` to try again"
                    .into(),
            ));
        }
        tokio::time::sleep(SESSION_CHECK_INTERVAL).await;
    }
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
    let expression = "typeof window.g_ck === 'string' ? window.g_ck : ''";
    let evaluated = cdp_command(
        socket,
        *id,
        "Runtime.evaluate",
        json!({
            "expression": expression,
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
    Authenticated,
    Waiting,
}

async fn validate_session(
    http: &reqwest::Client,
    site_url: &str,
    cookie: &str,
    user_token: &str,
) -> Result<SessionValidation, ApiError> {
    let response = http
        .get(format!("{site_url}/api/now/table/sys_user"))
        .query(&[
            ("sysparm_query", "sys_id=javascript:gs.getUserID()"),
            ("sysparm_fields", "sys_id,user_name,name"),
            ("sysparm_limit", "1"),
        ])
        .header(reqwest::header::COOKIE, cookie)
        .header("X-UserToken", user_token)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await?;
    if response.status().is_success() {
        return Ok(SessionValidation::Authenticated);
    }
    let logged_in = response
        .headers()
        .get("x-is-logged-in")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));
    match response.status().as_u16() {
        401 => Ok(SessionValidation::Waiting),
        403 if !logged_in => Ok(SessionValidation::Waiting),
        300..=399 => Ok(SessionValidation::Waiting),
        403 => Err(ApiError::Auth(
            "browser sign-in succeeded, but this account is not allowed to use the ServiceNow Table API"
                .into(),
        )),
        status => Err(ApiError::Other(format!(
            "browser-session validation returned HTTP {status}"
        ))),
    }
}

fn find_native_browser() -> Result<PathBuf, ApiError> {
    if let Some(browser) = std::env::var_os("SERVICENOW_BROWSER") {
        return resolve_program(Path::new(&browser)).ok_or_else(|| {
            ApiError::InvalidInput(format!(
                "SERVICENOW_BROWSER does not identify an executable: {}",
                Path::new(&browser).display()
            ))
        });
    }

    #[cfg(target_os = "macos")]
    let absolute = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ];
    #[cfg(not(target_os = "macos"))]
    let absolute: [&str; 0] = [];
    for candidate in absolute {
        if let Some(path) = resolve_program(Path::new(candidate)) {
            return Ok(path);
        }
    }
    for candidate in [
        "google-chrome",
        "google-chrome-stable",
        "microsoft-edge",
        "microsoft-edge-stable",
        "chromium",
        "chromium-browser",
    ] {
        if let Some(path) = find_in_path(candidate) {
            return Ok(path);
        }
    }
    Err(ApiError::InvalidInput(
        "browser sign-in needs Chrome, Edge, or Chromium; install one or set SERVICENOW_BROWSER to its executable"
            .into(),
    ))
}

fn resolve_program(candidate: &Path) -> Option<PathBuf> {
    if candidate.components().count() > 1 {
        candidate.is_file().then(|| candidate.to_path_buf())
    } else {
        find_in_path(candidate.as_os_str())
    }
}

fn find_in_path(candidate: impl AsRef<std::ffi::OsStr>) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|path| path.join(candidate.as_ref()))
            .find(|path| path.is_file())
    })
}

fn uses_windows_browser_bridge() -> bool {
    cfg!(target_os = "windows")
        || (cfg!(target_os = "linux")
            && (std::env::var_os("WSL_INTEROP").is_some()
                || std::fs::read_to_string("/proc/sys/kernel/osrelease")
                    .is_ok_and(|value| value.to_ascii_lowercase().contains("microsoft"))))
}

async fn windows_browser_cookie(site_url: &str) -> Result<BrowserSession, ApiError> {
    let powershell = ["powershell.exe", "pwsh.exe"]
        .into_iter()
        .find_map(find_in_path)
        .ok_or_else(|| {
            ApiError::InvalidInput(
                "WSL browser sign-in needs Windows PowerShell interop; ensure powershell.exe is available on PATH"
                    .into(),
            )
        })?;
    let browser_override = std::env::var_os("SERVICENOW_BROWSER");
    let bridge = render_windows_bridge(site_url, browser_override.as_deref());
    let encoded_bridge = powershell_encoded_command(&bridge);
    let child = tokio::process::Command::new(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            &encoded_bridge,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| ApiError::Other(format!("failed to start PowerShell: {error}")))?;
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| ApiError::Other(format!("browser bridge failed: {error}")))?;

    let stdout = decode_powershell_output(&output.stdout);
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
    let detail = decode_powershell_output(&output.stderr)
        .trim()
        .chars()
        .take(500)
        .collect::<String>();
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

fn powershell_encoded_command(script: &str) -> String {
    let utf16_le = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    base64::engine::general_purpose::STANDARD.encode(utf16_le)
}

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
}

const WINDOWS_BROWSER_BRIDGE: &str = r#"
$ErrorActionPreference = 'Stop'
Write-Output 'SERVICENOW_BRIDGE_READY'
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
$browser = ConvertFrom-HexUtf8 '__BROWSER_HEX__'
if (-not $browser) { $browser = $env:SERVICENOW_BROWSER }
if (-not $browser -or -not (Test-Path -LiteralPath $browser)) {
    $candidates = @(
        "$env:PROGRAMFILES\Microsoft\Edge\Application\msedge.exe",
        "${env:PROGRAMFILES(X86)}\Microsoft\Edge\Application\msedge.exe",
        "$env:LOCALAPPDATA\Microsoft\Edge\Application\msedge.exe",
        "$env:PROGRAMFILES\Google\Chrome\Application\chrome.exe",
        "${env:PROGRAMFILES(X86)}\Google\Chrome\Application\chrome.exe",
        "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe"
    )
    $browser = $candidates | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
}
if (-not $browser) { throw 'Chrome or Edge was not found on Windows. Set SERVICENOW_BROWSER to its Windows executable path.' }

$profile = Join-Path $env:TEMP ("servicenow-cli-browser-" + [Guid]::NewGuid().ToString('N'))
$process = $null
$socket = $null
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
    $quotedProfile = '"' + $profile + '"'
    $quotedUrl = '"' + $loginUrl + '"'
    $privateMode = if ((Split-Path -Leaf $browser) -match '(?i)msedge') { '--inprivate' } else { '--incognito' }
    $arguments = "--remote-debugging-port=0 --remote-debugging-address=127.0.0.1 --user-data-dir=$quotedProfile --no-first-run --no-default-browser-check --disable-sync $privateMode --new-window $quotedUrl"
    $process = Start-Process -FilePath $browser -ArgumentList $arguments -PassThru
    $deadline = [DateTime]::UtcNow.AddMinutes(5)
    $version = $null
    while (-not $version -and [DateTime]::UtcNow -lt $deadline) {
        if ($process.HasExited) { throw "Browser closed before sign-in completed ($($process.ExitCode))." }
        try {
            $portFile = Join-Path $profile 'DevToolsActivePort'
            if (Test-Path -LiteralPath $portFile) {
                $port = (Get-Content -LiteralPath $portFile -TotalCount 1).Trim()
                if ($port -match '^\d+$') {
                    $version = Invoke-RestMethod -UseBasicParsing "http://127.0.0.1:$port/json/version" -TimeoutSec 2
                }
            }
        } catch {}
        if (-not $version) { Start-Sleep -Milliseconds 200 }
    }
    if (-not $version.webSocketDebuggerUrl) { throw 'The private browser sign-in channel did not become available.' }

    $socket = [Net.WebSockets.ClientWebSocket]::new()
    $socket.ConnectAsync([Uri]$version.webSocketDebuggerUrl, [Threading.CancellationToken]::None).GetAwaiter().GetResult()
    $requestId = 0
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
                $cookieHeader = ($cookies | ForEach-Object { "$($_.name)=$($_.value)" }) -join '; '
                $evaluated = Invoke-CdpCommand $socket ([ref]$requestId) 'Runtime.evaluate' @{ expression = "typeof window.g_ck === 'string' ? window.g_ck : ''"; returnByValue = $true } $sessionId
                $userToken = $evaluated.result.result.value
                if ($userToken -and $userToken -notmatch '[\r\n]') {
                    try {
                        $response = Invoke-WebRequest -UseBasicParsing -Uri $apiUrl -Headers @{ Cookie = $cookieHeader; 'X-UserToken' = $userToken; Accept = 'application/json' } -MaximumRedirection 0 -TimeoutSec 10
                        if ($response.StatusCode -ge 200 -and $response.StatusCode -lt 300) {
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
    throw 'Browser sign-in timed out after five minutes.'
} finally {
    if ($socket) { try { $socket.Dispose() } catch {} }
    if ($process -and -not $process.HasExited) { try { Stop-Process -Id $process.Id -Force } catch {} }
    if (Test-Path -LiteralPath $profile) { try { Remove-Item -LiteralPath $profile -Recurse -Force } catch {} }
}
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
        assert!(WINDOWS_BROWSER_BRIDGE.contains("--inprivate"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("--incognito"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("Network.getCookies"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("'X-UserToken' = $userToken"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("$instance.Host"));
        assert!(WINDOWS_BROWSER_BRIDGE.contains("__INSTANCE_HEX__"));
        assert!(!WINDOWS_BROWSER_BRIDGE.contains("login.microsoftonline.com"));
        assert_eq!(
            powershell_hex(b"https://company.service-now.com"),
            "68747470733a2f2f636f6d70616e792e736572766963652d6e6f772e636f6d"
        );
        let rendered = render_windows_bridge("https://company.service-now.com", None);
        assert!(!rendered.contains("__INSTANCE_HEX__"));
        assert!(!rendered.contains("__BROWSER_HEX__"));
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
    fn powershell_bridge_is_encoded_as_one_complete_command() {
        use base64::engine::general_purpose::STANDARD;

        let script = "Write-Output 'SERVICENOW_BRIDGE_READY'";
        let decoded = STANDARD.decode(powershell_encoded_command(script)).unwrap();
        let (pairs, remainder) = decoded.as_chunks::<2>();
        assert!(remainder.is_empty());
        let units = pairs
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        assert_eq!(String::from_utf16(units.as_slice()).unwrap(), script);
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

    #[cfg(target_os = "windows")]
    #[test]
    fn encoded_powershell_command_executes_and_returns_output() {
        let powershell = find_in_path("powershell.exe").expect("powershell.exe on Windows");
        let output = std::process::Command::new(powershell)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand",
                &powershell_encoded_command("Write-Output 'SERVICENOW_BRIDGE_READY'"),
            ])
            .stdin(Stdio::null())
            .output()
            .unwrap();
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
    async fn cdp_session_is_validated_before_its_cookie_is_returned() {
        let instance = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/now/table/sys_user"))
            .and(header("cookie", "JSESSIONID=validated-session"))
            .and(header("x-usertoken", "synthetic-user-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [{"sys_id": "0123456789abcdef0123456789abcdef"}]
            })))
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
                    "Runtime.evaluate" => json!({"result": {
                        "type": "string",
                        "value": "synthetic-user-token"
                    }}),
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

        let session = wait_for_session_cookie(&format!("ws://{address}"), &instance.uri(), None)
            .await
            .unwrap();
        assert_eq!(session.cookie, "JSESSIONID=validated-session");
        assert_eq!(session.user_token, "synthetic-user-token");
        cdp.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "opens an installed Chromium browser"]
    async fn installed_browser_completes_a_local_session_handoff() {
        if find_native_browser().is_err() {
            return;
        }
        let instance = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/nav_to.do"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(
                        "<script>window.g_ck = 'browser-smoke-user-token'</script>Signed in",
                        "text/html; charset=utf-8",
                    )
                    .insert_header("set-cookie", "JSESSIONID=browser-smoke; Path=/; HttpOnly"),
            )
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
