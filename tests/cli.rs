use std::io::Read;
use std::process::{Command, Stdio};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;
use wiremock::matchers::{body_bytes, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn command(config_home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("servicenow").unwrap();
    command
        .env("XDG_CONFIG_HOME", config_home.path())
        .env(
            "SERVICENOW_CONFIG_DIR",
            config_home.path().join("servicenow"),
        )
        .env("XDG_CACHE_HOME", config_home.path().join("cache"))
        .env(
            "SERVICENOW_CACHE_DIR",
            config_home.path().join("cache/servicenow"),
        )
        .env_remove("SERVICENOW_INSTANCE")
        .env_remove("SERVICENOW_USERNAME")
        .env_remove("SERVICENOW_PASSWORD")
        .env_remove("SERVICENOW_COOKIE")
        .env_remove("SERVICENOW_USER_TOKEN")
        .env_remove("SERVICENOW_TOKEN")
        .env_remove("SERVICENOW_AUTH_TYPE")
        .env_remove("SERVICENOW_PROFILE")
        .env_remove("SERVICENOW_READ_ONLY")
        .env_remove("SERVICENOW_VERBOSE");
    command
}

/// Runs `args`, closing our end of its stdout pipe before it can write anything, then
/// returns its stderr and exit status. Reproduces "the reader went away" the same way
/// `head -c N` does: because the read end is fully closed before the child's first write,
/// even a small amount of output is enough to trigger a broken pipe.
fn run_with_stdout_closed_early(
    config_home: &TempDir,
    args: &[&str],
) -> (String, std::process::ExitStatus) {
    let mut child = command(config_home)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take().unwrap());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().unwrap();
    (stderr, status)
}

#[test]
fn schema_output_survives_a_closed_stdout_pipe() {
    let config_home = TempDir::new().unwrap();
    let (stderr, status) = run_with_stdout_closed_early(&config_home, &["schema"]);
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert_eq!(status.code(), Some(0));
}

#[test]
fn completions_output_survives_a_closed_stdout_pipe() {
    let config_home = TempDir::new().unwrap();
    let (stderr, status) = run_with_stdout_closed_early(&config_home, &["completions", "zsh"]);
    assert!(stderr.is_empty(), "stderr: {stderr}");
    assert_eq!(status.code(), Some(0));
}

#[test]
fn unknown_flag_under_json_output_uses_the_error_envelope() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .args(["--output", "json", "incidents", "list", "--bogus-flag"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "invalid_input");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--bogus-flag")
    );
}

#[test]
fn missing_required_argument_under_json_output_uses_the_error_envelope() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .args(["--output", "json", "tables", "get"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "invalid_input");
}

#[test]
fn unknown_subcommand_under_json_output_uses_the_error_envelope() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .args(["--output", "json", "bogus-subcommand"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "invalid_input");
}

#[test]
fn unknown_flag_under_text_output_keeps_the_plain_usage_error() {
    let config_home = TempDir::new().unwrap();
    for format in [&["--output", "text"][..], &["-otext"]] {
        let output = command(&config_home)
            .args(format)
            .args(["incidents", "list", "--bogus-flag"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{format:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("error: "), "{format:?}: {stderr}");
    }
}

#[test]
fn help_and_version_are_unaffected_by_json_output() {
    let config_home = TempDir::new().unwrap();
    command(&config_home)
        .args(["--output", "json", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Fast, safe, human-friendly"));
    command(&config_home)
        .args(["--output", "json", "--version"])
        .assert()
        .success()
        .stdout(predicate::str::contains("servicenow"));
}

#[test]
fn empty_csv_result_prints_nothing() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .args(["--output", "csv", "profile", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn tables_delete_refusal_message_matches_other_delete_commands() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .env("SERVICENOW_INSTANCE", "http://127.0.0.1:9")
        .env("SERVICENOW_USERNAME", "api-user")
        .env("SERVICENOW_PASSWORD", "secret")
        .args([
            "tables",
            "delete",
            "incident",
            "0123456789abcdef0123456789abcdef",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    let message = error["error"]["message"].as_str().unwrap();
    // Old behavior unconditionally refused with "rerun with --yes"; the fixed behavior
    // matches the cancellation wording `profile remove` and `attachments delete` already
    // use for a non-interactive refusal.
    assert!(message.contains("cancelled"), "message: {message}");
    assert!(message.contains("non-interactive"), "message: {message}");
}

#[test]
fn auth_login_help_advertises_secret_free_verbose_progress() {
    let config_home = TempDir::new().unwrap();
    command(&config_home)
        .args(["auth", "login", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Show secret-free browser sign-in progress on stderr",
        ));
}

#[test]
fn tui_help_describes_incident_operations_and_generic_browsing() {
    let config_home = TempDir::new().unwrap();
    command(&config_home)
        .args(["tui", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Operate incidents and browse ServiceNow tables interactively",
        ))
        .stdout(predicate::str::contains("Table to open first"))
        .stdout(predicate::str::contains("--page-size"));
}

#[test]
fn tui_refuses_non_interactive_streams_before_loading_credentials() {
    let config_home = TempDir::new().unwrap();
    command(&config_home)
        .arg("tui")
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "the TUI requires an interactive terminal on stdin and stdout",
        ));
}

#[test]
fn tui_rejects_an_unbounded_page_size() {
    let config_home = TempDir::new().unwrap();
    command(&config_home)
        .args(["tui", "--page-size", "4"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "page size must be between 5 and 200",
        ));
}

fn authenticated_command(config_home: &TempDir, server: &MockServer) -> Command {
    let mut command = command(config_home);
    command
        .env("SERVICENOW_INSTANCE", server.uri())
        .env("SERVICENOW_USERNAME", "admin")
        .env("SERVICENOW_PASSWORD", "secret");
    command
}

fn cache_incident_resolution_choices(config_home: &TempDir) {
    let path = config_home
        .path()
        .join("cache/servicenow/metadata/default/incident.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "table": "incident",
            "fetched_at": 1,
            "fields": [],
            "choices": {
                "state": [
                    {"value": "2", "label": "In Progress", "sequence": 20},
                    {"value": "9", "label": "Resolved", "sequence": 90},
                    {"value": "7", "label": "Closed", "sequence": 100}
                ],
                "close_code": [
                    {
                        "value": "solved_permanently",
                        "label": "Solved (Permanently)",
                        "sequence": 10
                    },
                    {
                        "value": "solved_workaround",
                        "label": "Solved (Work Around)",
                        "sequence": 20
                    }
                ]
            }
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn schema_is_offline_and_machine_readable() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home).arg("schema").output().unwrap();
    assert!(output.status.success());
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(schema["name"], "servicenow");
    assert_eq!(schema["schemaVersion"], "1.0");
    assert_eq!(schema["outputContract"]["listEnvelope"]["count"], "integer");
    assert!(schema["commands"].as_array().unwrap().len() >= 5);
}

#[test]
fn schema_can_describe_one_command_compactly() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .args(["schema", "--command", "incidents list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(schema["name"], "list");
    assert_eq!(schema["path"], "incidents list");
    assert_eq!(schema["schemaVersion"], "1.0");
    let arguments = schema["arguments"].as_array().unwrap();
    let display_value = arguments
        .iter()
        .find(|argument| argument["id"] == "display_value")
        .unwrap();
    assert_eq!(display_value["type"], "enum");
    assert_eq!(display_value["possibleValues"][2], "all");
    assert!(
        display_value["dynamicDefault"]
            .as_str()
            .unwrap()
            .contains("machine")
    );
}

#[test]
fn init_schema_describes_adaptive_onboarding_defaults() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .args(["schema", "--command", "init"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let method = schema["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|argument| argument["id"] == "method")
        .unwrap();
    assert!(
        method["possibleValues"]
            .as_array()
            .unwrap()
            .contains(&"browser".into())
    );
    assert!(
        method["possibleValues"]
            .as_array()
            .unwrap()
            .contains(&"oauth".into())
    );
    assert!(
        !method["possibleValues"]
            .as_array()
            .unwrap()
            .contains(&"o-auth".into())
    );
    let arguments = schema["arguments"].as_array().unwrap();
    let dynamic_default = |id: &str| {
        arguments
            .iter()
            .find(|argument| argument["id"] == id)
            .unwrap()["dynamicDefault"]
            .as_str()
            .unwrap()
    };
    assert!(dynamic_default("method").contains("detected"));
    assert!(dynamic_default("scope").contains("useraccount"));
    assert!(dynamic_default("redirect_uri").contains("8484"));
}

#[test]
fn schema_marks_destructive_and_dry_run_commands() {
    let config_home = TempDir::new().unwrap();
    let deletion = command(&config_home)
        .args(["schema", "--command", "attachments delete"])
        .output()
        .unwrap();
    let deletion: serde_json::Value = serde_json::from_slice(&deletion.stdout).unwrap();
    assert_eq!(deletion["behavior"]["destructive"], true);
    assert_eq!(deletion["behavior"]["requiresConfirmation"], true);
    assert_eq!(deletion["behavior"]["supportsDryRun"], true);

    let resolution = command(&config_home)
        .args(["schema", "--command", "incidents resolve"])
        .output()
        .unwrap();
    let resolution: serde_json::Value = serde_json::from_slice(&resolution.stdout).unwrap();
    assert_eq!(resolution["behavior"]["mutation"], true);
    assert_eq!(resolution["behavior"]["destructive"], false);
    assert_eq!(resolution["behavior"]["requiresConfirmation"], false);
    assert_eq!(resolution["behavior"]["supportsDryRun"], true);
    assert_eq!(resolution["argumentGroups"][0]["required"], true);
    assert_eq!(
        resolution["argumentGroups"][0]["arguments"],
        serde_json::json!(["notes", "notes_file"])
    );
    let state = resolution["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|argument| argument["id"] == "state")
        .unwrap();
    assert!(
        state["dynamicDefault"]
            .as_str()
            .unwrap()
            .contains("Resolved")
    );

    let tui = command(&config_home)
        .args(["schema", "--command", "tui"])
        .output()
        .unwrap();
    let tui: serde_json::Value = serde_json::from_slice(&tui.stdout).unwrap();
    assert_eq!(tui["behavior"]["mutation"], true);
    assert_eq!(tui["behavior"]["sideEffect"], "remote");
    assert_eq!(tui["behavior"]["requiresConfirmation"], true);
    assert_eq!(tui["behavior"]["supportsDryRun"], false);
}

#[tokio::test]
async fn incident_machine_output_keeps_raw_values_by_default() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(query_param("sysparm_display_value", "false"))
        .and(query_param(
            "sysparm_fields",
            "sys_id,number,short_description,state,priority,assigned_to,sys_updated_on",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{"sys_id": "0123456789abcdef0123456789abcdef", "number": "INC0010001", "state": "2"}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["incidents", "list", "--limit", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["count"], 1);
    assert_eq!(value["result"][0]["state"], "2");
    assert_eq!(
        value["result"][0]["sys_id"],
        "0123456789abcdef0123456789abcdef"
    );
}

#[tokio::test]
async fn successful_commands_persist_rotated_browser_cookies() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let config_dir = config_home.path().join("servicenow");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"active_profile = "work"

[profiles.work]
instance = "{}"
auth_type = "browser"
credential_store = "file"

[profiles.work.credential]
kind = "browser"
cookie = "JSESSIONID=initial-session"
user_token = "synthetic-user-token"
"#,
            server.uri()
        ),
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(header("cookie", "JSESSIONID=initial-session"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "JSESSIONID=rotated-session; Path=/; HttpOnly")
                .set_body_json(serde_json::json!({"result": []})),
        )
        .expect(1)
        .mount(&server)
        .await;

    command(&config_home)
        .args(["--profile", "work", "incidents", "list", "--limit", "1"])
        .assert()
        .success();

    let config = std::fs::read_to_string(config_dir.join("config.toml")).unwrap();
    assert!(config.contains("cookie = \"JSESSIONID=rotated-session\""));
    assert!(!config.contains("JSESSIONID=initial-session"));
}

#[tokio::test]
async fn rejected_commands_do_not_persist_response_cookies() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let config_dir = config_home.path().join("servicenow");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"active_profile = "work"

[profiles.work]
instance = "{}"
auth_type = "browser"
credential_store = "file"

[profiles.work.credential]
kind = "browser"
cookie = "JSESSIONID=last-known-good"
user_token = "synthetic-user-token"
"#,
            server.uri()
        ),
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header(
                    "set-cookie",
                    "JSESSIONID=rejected-session; Path=/; HttpOnly",
                )
                .set_body_json(serde_json::json!({
                    "error": {"message": "User is not authenticated", "detail": "Session expired"}
                })),
        )
        .expect(1)
        .mount(&server)
        .await;

    command(&config_home)
        .args(["--profile", "work", "incidents", "list", "--limit", "1"])
        .assert()
        .code(3);

    let config = std::fs::read_to_string(config_dir.join("config.toml")).unwrap();
    assert!(config.contains("cookie = \"JSESSIONID=last-known-good\""));
    assert!(!config.contains("JSESSIONID=rejected-session"));
}

#[tokio::test]
async fn incident_text_output_prefers_display_values_and_curated_columns() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(query_param("sysparm_display_value", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "sys_id": "0123456789abcdef0123456789abcdef",
                "number": "INC0010001",
                "priority": "1 - Critical",
                "short_description": "Email is unavailable",
                "state": "In Progress",
                "assigned_to": "Don Goodliffe",
                "sys_updated_on": "2026-08-22 10:30:00"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["--output", "table", "incidents", "list", "--limit", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("DESCRIPTION"));
    assert!(text.contains("ASSIGNEE"));
    assert!(text.contains("Don Goodliffe"));
    assert!(!text.contains("SYS ID"));
    assert!(!text.contains("0123456789abcdef0123456789abcdef"));
}

#[test]
fn config_init_does_not_require_credentials() {
    let config_home = TempDir::new().unwrap();
    command(&config_home)
        .args(["config", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("configPath"));
}

#[tokio::test]
async fn init_can_fall_back_to_the_protected_config_file() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "sys_id=javascript:gs.getUserID()",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "sys_id": "0123456789abcdef0123456789abcdef",
                "user_name": "admin",
                "name": "Admin User"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut init = assert_cmd::Command::from_std(command(&config_home));
    let output = init
        .args([
            "init",
            "--profile",
            "work",
            "--instance",
            &server.uri(),
            "--username",
            "admin",
            "--method",
            "basic",
            "--secret-stdin",
            "--insecure-storage",
        ])
        .write_stdin("secret\n")
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["credentialStore"], "config-file");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("stored in plaintext")
    );

    let config_path = config_home.path().join("servicenow/config.toml");
    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(config.contains("credential_store = \"file\""));
    assert!(config.contains("password = \"secret\""));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&config_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let shown = command(&config_home)
        .args(["--profile", "work", "config", "show"])
        .output()
        .unwrap();
    assert!(shown.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["secretMasked"], "***cret");

    command(&config_home)
        .args(["auth", "logout", "--profile", "work"])
        .assert()
        .success();
    let config = std::fs::read_to_string(config_path).unwrap();
    assert!(!config.contains("password = \"secret\""));
}

#[tokio::test]
async fn init_detects_microsoft_entra_and_selects_zero_admin_browser_sign_in() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/nav_to.do"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            "/auth_redirect.do?sysparm_url=https%3A%2F%2Flogin.microsoftonline.com%2Fexample%2Fsaml2",
        ))
        .expect(1)
        .mount(&server)
        .await;

    command(&config_home)
        .args([
            "init",
            "work",
            "--instance",
            &server.uri(),
            "--insecure-storage",
            "--no-browser",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Microsoft Entra SSO detected"))
        .stderr(predicate::str::contains(
            "no ServiceNow password or OAuth application is required",
        ))
        .stderr(predicate::str::contains("remove --no-browser"));
}

#[tokio::test]
async fn init_does_not_treat_a_local_login_form_as_basic_authentication() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/nav_to.do"))
        .and(query_param("uri", "incident_list.do"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/login.do"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/login.do"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<form>Password</form>"))
        .expect(1)
        .mount(&server)
        .await;

    command(&config_home)
        .args(["init", "work", "--instance", &server.uri()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "did not expose a definitive login method",
        ))
        .stderr(predicate::str::contains("username/password login detected").not());
}

#[tokio::test]
async fn rejected_basic_auth_on_federated_instance_recommends_browser_sign_in() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": {
                "message": "User is not authenticated",
                "detail": "Required to provide Auth information"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/nav_to.do"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            "/auth_redirect.do?sysparm_url=https%3A%2F%2Flogin.microsoftonline.com%2Ftenant-guid%2Fsaml2",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut init = assert_cmd::Command::from_std(command(&config_home));
    init.args([
        "init",
        "work",
        "--instance",
        &server.uri(),
        "--username",
        "federated-user",
        "--method",
        "basic",
        "--secret-stdin",
        "--insecure-storage",
    ])
    .write_stdin("not-a-servicenow-password\n")
    .assert()
    .failure()
    .stderr(predicate::str::contains("Microsoft Entra SSO"))
    .stderr(predicate::str::contains("Federated accounts"))
    .stderr(predicate::str::contains("--method browser"))
    .stderr(predicate::str::contains("no OAuth application"))
    .stderr(predicate::str::contains("Application Registry").not())
    .stderr(predicate::str::contains("servicenow auth login").not());
}

async fn delete_answered_with(status: u16, message: &str) -> (Option<i32>, serde_json::Value) {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path(
            "/api/now/table/sys_flow_log/0123456789abcdef0123456789abcdef",
        ))
        .respond_with(
            ResponseTemplate::new(status).set_body_json(serde_json::json!({
                "error": {"message": message, "detail": null},
                "status": "failure"
            })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let output = command(&config_home)
        .env("SERVICENOW_INSTANCE", server.uri())
        .env("SERVICENOW_USERNAME", "admin")
        .env("SERVICENOW_PASSWORD", "secret")
        .args([
            "table",
            "delete",
            "sys_flow_log",
            "0123456789abcdef0123456789abcdef",
            "--yes",
        ])
        .output()
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    (output.status.code(), envelope)
}

#[tokio::test]
async fn forbidden_responses_are_permission_errors_not_credential_problems() {
    let (code, envelope) = delete_answered_with(
        403,
        "Operation Failed: ACL Exception Delete Failed due to security constraints",
    )
    .await;

    assert_eq!(code, Some(3), "{envelope}");
    let error = &envelope["error"];
    assert_eq!(error["kind"], "permission", "{envelope}");
    assert_eq!(
        error["message"],
        "permission denied: Operation Failed: ACL Exception Delete Failed due to security constraints"
    );
    let remediation = error["remediation"].as_str().unwrap_or_default();
    assert!(remediation.contains("role or ACL"), "{envelope}");
    assert!(!remediation.contains("auth login"), "{envelope}");
}

#[tokio::test]
async fn unauthorized_responses_remain_credential_problems() {
    let (code, envelope) = delete_answered_with(401, "User Not Authenticated").await;

    assert_eq!(code, Some(3), "{envelope}");
    let error = &envelope["error"];
    assert_eq!(error["kind"], "auth", "{envelope}");
    assert_eq!(
        error["message"],
        "authentication failed: User Not Authenticated"
    );
    assert_eq!(
        error["remediation"],
        "run `servicenow auth login` to refresh your credentials"
    );
}

#[tokio::test]
async fn auth_login_resumes_a_saved_profile_without_reasking_for_the_instance() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let config_dir = config_home.path().join("servicenow");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "active_profile = \"work\"\n\n[profiles.work]\ninstance = \"{}\"\nauth_type = \"basic\"\n",
            server.uri()
        ),
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "sys_id": "0123456789abcdef0123456789abcdef",
                "user_name": "admin",
                "name": "Admin User"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut login = assert_cmd::Command::from_std(command(&config_home));
    login
        .args([
            "auth",
            "login",
            "--profile",
            "work",
            "--username",
            "admin",
            "--method",
            "basic",
            "--secret-stdin",
            "--insecure-storage",
        ])
        .write_stdin("secret\n")
        .assert()
        .success()
        .stderr(predicate::str::contains("Resuming profile 'work'"));
}

#[test]
fn profile_selection_rejects_ambiguous_global_and_legacy_inputs() {
    let config_home = TempDir::new().unwrap();
    command(&config_home)
        .args(["init", "legacy", "--profile", "preferred"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains(
            "--profile or the legacy positional argument, not both",
        ));
}

#[test]
fn offline_auth_status_and_doctor_do_not_require_instance_access() {
    let config_home = TempDir::new().unwrap();
    let mut offline = command(&config_home);
    offline
        .env("SERVICENOW_INSTANCE", "offline.invalid")
        .env("SERVICENOW_USERNAME", "api-user")
        .env("SERVICENOW_PASSWORD", "secret");

    let status = offline
        .args(["auth", "status", "--offline"])
        .output()
        .unwrap();
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["status"], "configured");
    assert_eq!(status["verified"], false);
    assert_eq!(status["credentialStore"], "environment");

    let doctor = command(&config_home)
        .env("SERVICENOW_INSTANCE", "offline.invalid")
        .env("SERVICENOW_USERNAME", "api-user")
        .env("SERVICENOW_PASSWORD", "secret")
        .args(["doctor", "--offline"])
        .output()
        .unwrap();
    assert!(doctor.status.success());
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctor["offline"], true);
    assert_eq!(doctor["verified"], false);
    assert_eq!(doctor["checks"][3]["name"], "table_api");
    assert_eq!(doctor["checks"][3]["skipped"], true);
    assert!(doctor["checks"][3].get("ok").is_none());
    assert_eq!(doctor["checks"][3]["detail"], "network check skipped");
}

#[tokio::test]
async fn auth_status_verifies_the_current_identity_without_reading_incidents() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "sys_id=javascript:gs.getUserID()",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "sys_id": "0123456789abcdef0123456789abcdef",
                "user_name": "admin",
                "name": "System Administrator"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["auth", "status"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "ok");
    assert_eq!(result["verified"], true);
    assert_eq!(result["identity"], "admin");
}

#[tokio::test]
async fn auth_status_uses_the_display_name_when_username_is_missing() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "sys_id": "0123456789abcdef0123456789abcdef",
                "name": "System Administrator"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["auth", "status"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["identity"], "System Administrator");
}

#[test]
fn auth_status_and_doctor_schema_mark_network_access_as_conditional() {
    let config_home = TempDir::new().unwrap();
    for path in ["auth status", "doctor"] {
        let output = command(&config_home)
            .args(["schema", "--command", path])
            .output()
            .unwrap();
        assert!(output.status.success());
        let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(schema["behavior"]["networkAccess"], "conditional");
    }
}

#[test]
fn missing_config_has_structured_error_and_input_exit_code() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .args(["config", "show"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "invalid_input");
}

#[test]
fn config_show_masks_password() {
    let config_home = TempDir::new().unwrap();
    let config_dir = config_home.path().join("servicenow");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        r#"[default]
instance = "dev12345"
username = "api-user"
password = "very-secret"
"#,
    )
    .unwrap();

    let output = command(&config_home)
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["secretMasked"], "***cret");
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("very-secret")
    );
}

#[test]
fn delete_requires_explicit_confirmation_before_network_access() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .env("SERVICENOW_INSTANCE", "http://127.0.0.1:9")
        .env("SERVICENOW_USERNAME", "api-user")
        .env("SERVICENOW_PASSWORD", "secret")
        .args([
            "tables",
            "delete",
            "incident",
            "0123456789abcdef0123456789abcdef",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--yes")
    );
}

#[tokio::test]
async fn doctor_verifies_authentication_and_table_access() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "sys_id=javascript:gs.getUserID()",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "sys_id": "0123456789abcdef0123456789abcdef",
                "user_name": "admin",
                "name": "System Administrator",
                "active": "true"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": []
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = command(&config_home)
        .env("SERVICENOW_INSTANCE", server.uri())
        .env("SERVICENOW_USERNAME", "admin")
        .env("SERVICENOW_PASSWORD", "secret")
        .arg("doctor")
        .output()
        .unwrap();

    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["checks"][1]["name"], "authentication");
    assert_eq!(result["checks"][1]["detail"], "admin");
    assert_eq!(result["checks"][2]["name"], "credentials");
    assert_eq!(result["checks"][2]["detail"], "environment variable");
    assert_eq!(result["checks"][3]["name"], "table_api");
}

#[tokio::test]
async fn table_lists_support_json_lines_yaml_and_csv() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{"number": "INC0010001", "state": "2"}]
        })))
        .expect(3)
        .mount(&server)
        .await;

    let jsonl = authenticated_command(&config_home, &server)
        .args([
            "--output", "jsonl", "tables", "list", "incident", "--limit", "1",
        ])
        .output()
        .unwrap();
    assert!(jsonl.status.success());
    let row: serde_json::Value = serde_json::from_slice(&jsonl.stdout).unwrap();
    assert_eq!(row["number"], "INC0010001");

    let yaml = authenticated_command(&config_home, &server)
        .args([
            "--output", "yaml", "tables", "list", "incident", "--limit", "1",
        ])
        .output()
        .unwrap();
    assert!(yaml.status.success());
    let yaml = String::from_utf8(yaml.stdout).unwrap();
    assert!(yaml.contains("number: INC0010001"));

    let csv = authenticated_command(&config_home, &server)
        .args([
            "--output", "csv", "tables", "list", "incident", "--limit", "1",
        ])
        .output()
        .unwrap();
    assert!(csv.status.success());
    let csv = String::from_utf8(csv.stdout).unwrap();
    assert!(csv.contains("number,state"));
    assert!(csv.contains("INC0010001,2"));
}

#[tokio::test]
async fn schema_refresh_caches_dictionary_and_choices() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_dictionary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "element": "state",
                "column_label": "State",
                "internal_type": "integer",
                "reference": "",
                "mandatory": "true",
                "read_only": "false"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_choice"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "element": "state", "value": "2", "label": "In Progress", "sequence": "20"
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["schema", "incident", "--refresh"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["result"]["fields"][0]["name"], "state");
    assert_eq!(
        result["result"]["choices"]["state"][0]["label"],
        "In Progress"
    );
    assert!(
        config_home
            .path()
            .join("cache/servicenow/metadata/default/incident.json")
            .exists()
    );
}

#[tokio::test]
async fn incident_edit_applies_only_changed_fields() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let sys_id = "0123456789abcdef0123456789abcdef";
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{sys_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "sys_id": sys_id,
                "short_description": "Before",
                "state": {"value": "2", "display_value": "In Progress"}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("/api/now/table/incident/{sys_id}")))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "short_description": "After"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {"sys_id": sys_id, "number": "INC0010001", "short_description": "After"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let edit = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(edit.path(), "short_description: After\nstate: '2'\n").unwrap();

    let output = authenticated_command(&config_home, &server)
        .args([
            "incidents",
            "edit",
            sys_id,
            "--file",
            edit.path().to_str().unwrap(),
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["result"]["short_description"], "After");
}

#[tokio::test]
async fn incident_resolve_dry_run_maps_configured_labels_without_patching() {
    let config_home = TempDir::new().unwrap();
    cache_incident_resolution_choices(&config_home);
    let server = MockServer::start().await;
    let sys_id = "0123456789abcdef0123456789abcdef";
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{sys_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "sys_id": sys_id,
                "number": "INC0010001",
                "state": {"value": "2", "display_value": "In Progress"}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .env("SERVICENOW_READ_ONLY", "true")
        .args([
            "incidents",
            "resolve",
            sys_id,
            "--code",
            "solved (permanently)",
            "--notes",
            "Restored the VPN gateway configuration.",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["operation"], "resolve");
    assert_eq!(result["changes"]["state"], "9");
    assert_eq!(result["changes"]["close_code"], "solved_permanently");
    assert_eq!(
        result["changes"]["close_notes"],
        "Restored the VPN gateway configuration."
    );
}

#[tokio::test]
async fn incident_resolve_patches_resolution_atomically_with_custom_fields() {
    let config_home = TempDir::new().unwrap();
    cache_incident_resolution_choices(&config_home);
    let server = MockServer::start().await;
    let sys_id = "0123456789abcdef0123456789abcdef";
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{sys_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "sys_id": sys_id,
                "number": "INC0010001",
                "state": {"value": "2", "display_value": "In Progress"}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("/api/now/table/incident/{sys_id}")))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "state": "9",
            "close_code": "solved_workaround",
            "close_notes": "Temporary route restored service.\n",
            "u_resolution_source": "network_runbook"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "sys_id": sys_id,
                "number": "INC0010001",
                "state": "9",
                "close_code": "solved_workaround"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let notes = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(notes.path(), "Temporary route restored service.\n").unwrap();

    let output = authenticated_command(&config_home, &server)
        .args([
            "incidents",
            "resolve",
            sys_id,
            "--code",
            "Solved (Work Around)",
            "--notes-file",
            notes.path().to_str().unwrap(),
            "--field",
            "u_resolution_source=network_runbook",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["result"]["state"], "9");
    assert_eq!(result["result"]["close_code"], "solved_workaround");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Resolved"));
}

#[tokio::test]
async fn incident_resolve_refuses_closed_incidents() {
    let config_home = TempDir::new().unwrap();
    cache_incident_resolution_choices(&config_home);
    let server = MockServer::start().await;
    let sys_id = "0123456789abcdef0123456789abcdef";
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{sys_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "sys_id": sys_id,
                "number": "INC0010001",
                "state": {"value": "7", "display_value": "Closed"}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    authenticated_command(&config_home, &server)
        .args([
            "incidents",
            "resolve",
            sys_id,
            "--code",
            "solved_permanently",
            "--notes",
            "Already closed.",
        ])
        .assert()
        .code(7)
        .stderr(predicate::str::contains("already Closed"));
}

#[tokio::test]
async fn incident_resolve_requires_non_empty_notes_before_network_access() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    authenticated_command(&config_home, &server)
        .args([
            "incidents",
            "resolve",
            "INC0010001",
            "--code",
            "solved_permanently",
            "--notes",
            "   ",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("resolution notes cannot be empty"));
}

#[tokio::test]
async fn incident_note_dry_run_never_patches() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let sys_id = "0123456789abcdef0123456789abcdef";
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{sys_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {"sys_id": sys_id}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .env("SERVICENOW_READ_ONLY", "true")
        .args(["incidents", "note", sys_id, "Investigating", "--dry-run"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["dryRun"], true);
    assert_eq!(result["changes"]["work_notes"], "Investigating");
}

#[tokio::test]
async fn incident_assign_resolves_human_user_before_patching() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let incident_id = "0123456789abcdef0123456789abcdef";
    let user_id = "fedcba9876543210fedcba9876543210";
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "user_name=ada@example.com^ORemail=ada@example.com^ORname=ada@example.com",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{"sys_id": user_id, "user_name": "ada", "email": "ada@example.com"}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{incident_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {"sys_id": incident_id}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("/api/now/table/incident/{incident_id}")))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "assigned_to": user_id
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {"sys_id": incident_id, "number": "INC0010001", "assigned_to": user_id}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "incidents",
            "assign",
            incident_id,
            "--to",
            "ada@example.com",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["result"]["assigned_to"], user_id);
}

#[tokio::test]
async fn incident_watch_is_a_bounded_json_event_stream() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let sys_id = "0123456789abcdef0123456789abcdef";
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/incident/{sys_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {"sys_id": sys_id, "number": "INC0010001", "state": "2"}
        })))
        .expect(2)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "incidents",
            "watch",
            sys_id,
            "--interval",
            "1",
            "--count",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout).unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(events.trim()).unwrap();
    assert_eq!(snapshot["event"], "snapshot");
    assert_eq!(snapshot["record"]["number"], "INC0010001");
}

#[tokio::test]
async fn attachment_commands_cover_the_complete_file_lifecycle() {
    let config_home = TempDir::new().unwrap();
    let files = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let record_id = "0123456789abcdef0123456789abcdef";
    let attachment_id = "fedcba9876543210fedcba9876543210";
    let metadata = serde_json::json!({
        "sys_id": attachment_id,
        "file_name": "diagnostic.txt",
        "content_type": "text/plain",
        "size_bytes": "11",
        "table_name": "incident",
        "table_sys_id": record_id,
        "download_link": format!("{}/api/now/attachment/{attachment_id}/file", server.uri()),
        "sys_created_by": "admin",
        "sys_created_on": "2026-08-22 20:00:00"
    });

    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .and(query_param("sysparm_query", "number=INC0010001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{"sys_id": record_id, "number": "INC0010001"}]
        })))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/attachment"))
        .and(query_param(
            "sysparm_query",
            format!("table_name=incident^table_sys_id={record_id}^ORDERBYDESCsys_created_on"),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"result": [metadata.clone()]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/now/attachment/file"))
        .and(query_param("table_name", "incident"))
        .and(query_param("table_sys_id", record_id))
        .and(query_param("file_name", "diagnostic.txt"))
        .and(header("content-type", "text/plain"))
        .and(body_bytes(b"hello world"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(serde_json::json!({"result": metadata.clone()})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/attachment/{attachment_id}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"result": metadata.clone()})),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/attachment/{attachment_id}/file")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello world"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/now/attachment/{attachment_id}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let listed = authenticated_command(&config_home, &server)
        .args(["attachments", "list", "incident", "INC0010001"])
        .output()
        .unwrap();
    assert!(listed.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["count"], 1);
    assert_eq!(listed["result"][0]["file_name"], "diagnostic.txt");

    let source = files.path().join("diagnostic.txt");
    std::fs::write(&source, b"hello world").unwrap();
    let uploaded = authenticated_command(&config_home, &server)
        .args([
            "attachments",
            "upload",
            "incident",
            "INC0010001",
            source.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        uploaded.status.success(),
        "{}",
        String::from_utf8_lossy(&uploaded.stderr)
    );
    let uploaded: serde_json::Value = serde_json::from_slice(&uploaded.stdout).unwrap();
    assert_eq!(uploaded["result"]["sys_id"], attachment_id);

    let destination = files.path().join("downloaded.txt");
    let downloaded = authenticated_command(&config_home, &server)
        .args([
            "attachments",
            "download",
            attachment_id,
            destination.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        downloaded.status.success(),
        "{}",
        String::from_utf8_lossy(&downloaded.stderr)
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"hello world");
    let downloaded: serde_json::Value = serde_json::from_slice(&downloaded.stdout).unwrap();
    assert_eq!(downloaded["sizeBytes"], 11);

    let deleted = authenticated_command(&config_home, &server)
        .args(["attachments", "delete", attachment_id, "--yes"])
        .output()
        .unwrap();
    assert!(
        deleted.status.success(),
        "{}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    let deleted: serde_json::Value = serde_json::from_slice(&deleted.stdout).unwrap();
    assert_eq!(deleted["deleted"], true);
}

#[test]
fn attachment_delete_is_blocked_by_read_only_mode_before_network_access() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .env("SERVICENOW_INSTANCE", "http://127.0.0.1:9")
        .env("SERVICENOW_USERNAME", "api-user")
        .env("SERVICENOW_PASSWORD", "secret")
        .env("SERVICENOW_READ_ONLY", "true")
        .args([
            "attachments",
            "delete",
            "0123456789abcdef0123456789abcdef",
            "--yes",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "invalid_input");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("read-only")
    );
}

#[tokio::test]
async fn a_conflict_response_exits_with_the_conflict_code() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
            "error": {"message": "Duplicate key", "detail": "Record already exists"}
        })))
        .mount(&server)
        .await;

    authenticated_command(&config_home, &server)
        .args(["incidents", "list", "--limit", "1"])
        .assert()
        .code(7)
        .stderr(predicate::str::contains("Record already exists"));
}

#[tokio::test]
async fn a_rate_limited_response_exits_with_the_rate_limit_code() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/incident"))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
            "error": {"message": "Rate limit exceeded", "detail": "Too many requests"}
        })))
        .mount(&server)
        .await;

    authenticated_command(&config_home, &server)
        .args(["incidents", "list", "--limit", "1"])
        .assert()
        .code(6)
        .stderr(predicate::str::contains("rate limit"));
}

const AWS_ITEM: &str = "5a1efa5947520510f53d37d2846d439b";

fn catalog_fixture(name: &str) -> serde_json::Value {
    let path = format!(
        "{}/tests/fixtures/catalog/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

async fn mount_aws_item(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{AWS_ITEM}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(catalog_fixture("aws_account_request")),
        )
        .mount(server)
        .await;
}

async fn forbid_order_post(server: &MockServer) {
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(server)
        .await;
}

const AWS_VARS: [&str; 6] = [
    "--var",
    "division=IT",
    "--var",
    "environment=Dev",
    "--var",
    "describe_briefly_the_purpose_of_this_account=Sandbox",
];

fn set_variable_default(node: &mut serde_json::Value, name: &str, value: &str) -> bool {
    match node {
        serde_json::Value::Object(map) => {
            if map.get("name").and_then(serde_json::Value::as_str) == Some(name) {
                map.insert("value".into(), value.into());
                return true;
            }
            map.values_mut()
                .any(|child| set_variable_default(child, name, value))
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .any(|child| set_variable_default(child, name, value)),
        _ => false,
    }
}

#[tokio::test]
async fn catalog_show_redacts_sensitive_defaults() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let mut zoom = catalog_fixture("zoom_webinar");
    assert!(set_variable_default(&mut zoom, "set_password", "hunter2"));
    let zoom_id = zoom["result"]["sys_id"].as_str().unwrap().to_string();
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{zoom_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(zoom))
        .expect(2)
        .mount(&server)
        .await;

    for format in ["json", "text"] {
        let output = authenticated_command(&config_home, &server)
            .args(["catalog", "show", &zoom_id, "--output", format])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!stdout.contains("hunter2"), "{format}: {stdout}");
        assert!(stdout.contains("[REDACTED]"), "{format}: {stdout}");
    }
}

#[tokio::test]
async fn catalog_order_dry_run_validates_without_submitting() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    forbid_order_post(&server).await;

    let output = authenticated_command(&config_home, &server)
        .env("SERVICENOW_READ_ONLY", "true")
        .args(["catalog", "order", AWS_ITEM, "--dry-run", "--json"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["dryRun"], true);
    assert_eq!(plan["operation"], "order");
    assert_eq!(plan["item"]["sys_id"], AWS_ITEM);
    assert_eq!(plan["variables"]["division"], "IT");
    assert_eq!(plan["variables"]["environment"], "Dev");
}

#[tokio::test]
async fn catalog_order_names_custom_widgets_it_cannot_fill() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let mut item = catalog_fixture("aws_account_request");
    item["result"]["variables"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "name": "cost_center_picker",
            "label": "Cost center",
            "friendly_type": "macro_with_abel",
            "type": 17
        }));
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{AWS_ITEM}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(item))
        .mount(&server)
        .await;
    forbid_order_post(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--dry-run", "--json"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("custom widgets the CLI cannot show or fill (Cost center)"),
        "{stderr}"
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(plan["variables"].get("cost_center_picker").is_none());
}

/// The AWS item with an optional multi-row variable set added.
async fn mount_aws_item_with_multi_row_set(server: &MockServer, mandatory: bool) {
    let mut item = catalog_fixture("aws_account_request");
    item["result"]["variables"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "name": "extra_users",
            "label": "Extra users",
            "friendly_type": "sc_multi_row",
            "type": "sc_multi_row",
            "mandatory": mandatory,
            "children": []
        }));
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{AWS_ITEM}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(item))
        .mount(server)
        .await;
}

#[tokio::test]
async fn catalog_order_leaves_an_optional_multi_row_set_empty() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let request_id = "b54ae46e1b181310b97aa975624bcbf2";
    mount_aws_item_with_multi_row_set(&server, false).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{AWS_ITEM}/order_now"
        )))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "sysparm_quantity": "1",
            "variables": {
                "division": "IT",
                "environment": "Dev",
                "describe_briefly_the_purpose_of_this_account": "Sandbox"
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "sys_id": request_id,
                "number": "REQ0010001",
                "request_number": "REQ0010001",
                "request_id": request_id,
                "table": "sc_request"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_req_item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{"number": "RITM0010001", "sys_id": "0123456789abcdef0123456789abcdef"}]
        })))
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes", "--json"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("warning: extra_users: optional multi-row variable set left empty"),
        "{stderr}"
    );
}

#[tokio::test]
async fn catalog_order_refuses_a_mandatory_multi_row_set_without_posting() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item_with_multi_row_set(&server, true).await;
    forbid_order_post(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--dry-run"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("mandatory multi-row variable set (extra_users)"),
        "{stderr}"
    );
    assert!(stderr.contains("/sp?id=sc_cat_item&sys_id="), "{stderr}");
}

#[tokio::test]
async fn catalog_order_refuses_rows_for_a_multi_row_set_without_posting() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item_with_multi_row_set(&server, false).await;
    forbid_order_post(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes"])
        .args(AWS_VARS)
        .args(["--var", "extra_users=[]"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("extra_users: multi-row variable sets cannot be filled by the CLI"),
        "{stderr}"
    );
}

#[tokio::test]
async fn catalog_show_names_optional_multi_row_sets_left_empty() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item_with_multi_row_set(&server, false).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "show", AWS_ITEM, "--output", "text"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains("these optional ones are left empty when ordering: extra_users."),
        "{stdout}"
    );
    assert!(!stdout.contains("Not orderable"), "{stdout}");
}

#[tokio::test]
async fn catalog_order_is_blocked_by_read_only_mode() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    forbid_order_post(&server).await;

    let output = authenticated_command(&config_home, &server)
        .env("SERVICENOW_READ_ONLY", "true")
        .args(["catalog", "order", AWS_ITEM, "--yes"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("read-only"));
}

#[tokio::test]
async fn catalog_order_without_yes_refuses_a_non_interactive_stdin() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    forbid_order_post(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM])
        .args(AWS_VARS)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("order cancelled"));
}

#[tokio::test]
async fn catalog_order_rejects_invalid_variables_before_submitting() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    forbid_order_post(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes"])
        .args(["--var", "division=Legal", "--var", "enviroment=Dev"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("division"), "{stderr}");
    assert!(stderr.contains("environment"), "{stderr}");
    assert!(
        stderr.contains("describe_briefly_the_purpose_of_this_account"),
        "{stderr}"
    );
}

#[tokio::test]
async fn catalog_order_submits_once_and_reports_the_requested_items() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let request_id = "b54ae46e1b181310b97aa975624bcbf2";
    mount_aws_item(&server).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{AWS_ITEM}/order_now"
        )))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "sysparm_quantity": "1",
            "variables": {
                "division": "IT",
                "environment": "Dev",
                "describe_briefly_the_purpose_of_this_account": "Sandbox"
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "sys_id": request_id,
                "number": "REQ0010001",
                "request_number": "REQ0010001",
                "request_id": request_id,
                "table": "sc_request"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_req_item"))
        .and(query_param(
            "sysparm_query",
            format!("request={request_id}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{"number": "RITM0010001", "sys_id": "0123456789abcdef0123456789abcdef"}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes", "--json"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["request"]["request_number"], "REQ0010001");
    assert_eq!(result["requestedItems"][0]["number"], "RITM0010001");
}

#[tokio::test]
async fn catalog_order_succeeds_when_the_follow_up_lookup_fails() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let request_id = "b54ae46e1b181310b97aa975624bcbf2";
    mount_aws_item(&server).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{AWS_ITEM}/order_now"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "sys_id": request_id,
                "number": "REQ0010001",
                "request_number": "REQ0010001",
                "request_id": request_id,
                "table": "sc_request"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_req_item"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": {"message": "Insufficient rights", "detail": ""},
            "status": "failure"
        })))
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes", "--json"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("do not reorder"), "{stderr}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["request"]["request_number"], "REQ0010001");
    assert!(result["requestedItems"].is_null(), "{result}");
}

/// Mounts an `order_now` answer that must be requested exactly once, and
/// fails the test if the CLI then looks for requested items.
async fn mount_order_answer(server: &MockServer, result: serde_json::Value) {
    mount_aws_item(server).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{AWS_ITEM}/order_now"
        )))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "result": result })),
        )
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_req_item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"result": []})))
        .expect(0)
        .mount(server)
        .await;
}

const CART_ID: &str = "90ed51b23b50dbd0b8094b44c3e45aad";

#[tokio::test]
async fn catalog_order_that_only_fills_a_cart_is_reported_as_not_ordered() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_order_answer(&server, serde_json::json!({ "cart_id": CART_ID })).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes", "--json"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(8), "{stderr}");
    assert!(!stderr.contains("Ordered"), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let error: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(error["error"]["kind"], "not_ordered", "{error}");
    assert_eq!(error["error"]["cart"]["sys_id"], CART_ID, "{error}");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("created no request"), "{message}");
    assert!(message.contains(CART_ID), "{message}");
    let remediation = error["error"]["remediation"].as_str().unwrap();
    assert!(remediation.contains("do not retry"), "{remediation}");
}

#[tokio::test]
async fn catalog_order_that_only_fills_a_cart_never_claims_success_in_text_mode() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_order_answer(&server, serde_json::json!({ "cart_id": CART_ID })).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes", "--output", "text"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(8), "{stderr}");
    assert!(
        !stdout.contains("Ordered") && !stderr.contains("Ordered"),
        "{stdout}{stderr}"
    );
    assert!(!stdout.contains("the request"), "{stdout}");
    assert!(stderr.contains("Nothing was ordered"), "{stderr}");
}

#[tokio::test]
async fn catalog_order_with_an_unrecognized_answer_names_its_keys_but_no_values() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_order_answer(
        &server,
        serde_json::json!({ "status": "queued-7f3a9c", "token": "s3cr3t-value" }),
    )
    .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes", "--json"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("Ordered"), "{stderr}");
    assert!(
        !stderr.contains("queued-7f3a9c") && !stderr.contains("s3cr3t-value"),
        "{stderr}"
    );
    let error: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("status, token"), "{message}");
    assert!(message.contains("may have been placed"), "{message}");
}

#[tokio::test]
async fn catalog_order_with_an_empty_answer_is_not_a_success() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_order_answer(&server, serde_json::json!({})).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes", "--json"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("Ordered"), "{stderr}");
    assert!(stderr.contains("no fields"), "{stderr}");
}

#[tokio::test]
async fn catalog_order_without_a_request_number_names_the_request_by_sys_id() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let request_id = "b54ae46e1b181310b97aa975624bcbf2";
    mount_aws_item(&server).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{AWS_ITEM}/order_now"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": { "request_id": request_id }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_req_item"))
        .and(query_param(
            "sysparm_query",
            format!("request={request_id}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"result": []})))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes", "--output", "text"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "Ordered AWS account request: request {request_id}"
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("the request"), "{stderr}");
}

fn aws_item_the_instance_calls(orderable: bool) -> serde_json::Value {
    let mut item = catalog_fixture("aws_account_request");
    item["result"]["orderable"] = orderable.into();
    item
}

fn unorderable_aws_item() -> serde_json::Value {
    aws_item_the_instance_calls(false)
}

#[tokio::test]
async fn catalog_show_reports_an_item_the_instance_calls_unorderable() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{AWS_ITEM}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(unorderable_aws_item()))
        .mount(&server)
        .await;

    let text = authenticated_command(&config_home, &server)
        .args(["catalog", "show", AWS_ITEM, "--output", "text"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    assert!(
        stdout.contains("the instance reports this item as not orderable through the API"),
        "{stdout}"
    );

    let json = authenticated_command(&config_home, &server)
        .args(["catalog", "show", AWS_ITEM, "--json"])
        .output()
        .unwrap();
    let shown: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(shown["result"]["instance_orderable"], false, "{shown}");
}

#[tokio::test]
async fn catalog_show_says_nothing_extra_when_the_instance_calls_an_item_orderable() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{AWS_ITEM}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(aws_item_the_instance_calls(true)))
        .mount(&server)
        .await;

    let text = authenticated_command(&config_home, &server)
        .args(["catalog", "show", AWS_ITEM, "--output", "text"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    assert!(!stdout.contains("not orderable"), "{stdout}");

    let json = authenticated_command(&config_home, &server)
        .args(["catalog", "show", AWS_ITEM, "--json"])
        .output()
        .unwrap();
    let shown: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(shown["result"]["instance_orderable"], true, "{shown}");
}

#[tokio::test]
async fn catalog_order_dry_run_warns_about_an_item_the_instance_calls_unorderable() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{AWS_ITEM}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(unorderable_aws_item()))
        .mount(&server)
        .await;
    forbid_order_post(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--dry-run", "--json"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let warnings = plan["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("not orderable through the API"))),
        "{plan}"
    );
}

#[tokio::test]
async fn catalog_order_warns_before_ordering_an_item_the_instance_calls_unorderable() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{AWS_ITEM}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(unorderable_aws_item()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{AWS_ITEM}/order_now"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": { "cart_id": CART_ID }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes", "--output", "text"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(8), "{stderr}");
    let warning = stderr
        .find("warning: the instance reports this item as not orderable through the API")
        .unwrap_or_else(|| panic!("no warning: {stderr}"));
    let error = stderr
        .find("error: ")
        .unwrap_or_else(|| panic!("no error: {stderr}"));
    assert!(
        warning < error,
        "the warning must precede the outcome: {stderr}"
    );
}

#[tokio::test]
async fn catalog_order_with_only_a_request_number_names_it_without_claiming_success() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_order_answer(
        &server,
        serde_json::json!({ "request_number": "REQ0010042" }),
    )
    .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--yes", "--json"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("Ordered"), "{stderr}");
    let error: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("named request REQ0010042"), "{message}");
    assert!(message.contains("may have been placed"), "{message}");
}

async fn mount_catalog_search_page(
    server: &MockServer,
    offset: usize,
    items: Vec<serde_json::Value>,
) {
    Mock::given(method("GET"))
        .and(path("/api/sn_sc/servicecatalog/items"))
        .and(query_param("sysparm_text", "AWS account request"))
        .and(query_param("sysparm_offset", offset.to_string()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "result": items })),
        )
        .expect(1)
        .mount(server)
        .await;
}

fn aws_search_page(exact_sys_id: Option<&str>, fillers: usize) -> Vec<serde_json::Value> {
    let mut items: Vec<serde_json::Value> = (0..fillers)
        .map(|index| {
            serde_json::json!({
                "sys_id": format!("{index:032x}"),
                "name": format!("AWS account request add-on {index}"),
                "sys_class_name": "sc_cat_item"
            })
        })
        .collect();
    if let Some(sys_id) = exact_sys_id {
        items.push(serde_json::json!({
            "sys_id": sys_id,
            "name": "AWS account request",
            "sys_class_name": "sc_cat_item"
        }));
    }
    items
}

#[tokio::test]
async fn catalog_order_finds_a_second_exact_match_beyond_the_first_page() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    forbid_order_post(&server).await;
    let other = "ffffffffffffffffffffffffffffffff";
    mount_catalog_search_page(&server, 0, aws_search_page(Some(AWS_ITEM), 99)).await;
    mount_catalog_search_page(&server, 100, aws_search_page(Some(other), 3)).await;

    let order = authenticated_command(&config_home, &server)
        .args(["catalog", "order", "AWS account request", "--dry-run"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(!order.status.success());
    let stderr = String::from_utf8_lossy(&order.stderr);
    assert!(stderr.contains("2 catalog items are named"), "{stderr}");
    assert!(
        stderr.contains(AWS_ITEM) && stderr.contains(other),
        "{stderr}"
    );
}

#[tokio::test]
async fn catalog_order_refuses_a_name_it_cannot_check_for_uniqueness() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    forbid_order_post(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/sn_sc/servicecatalog/items"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "result": aws_search_page(Some(AWS_ITEM), 99) }),
            ),
        )
        .expect(10)
        .mount(&server)
        .await;

    let order = authenticated_command(&config_home, &server)
        .args(["catalog", "order", "AWS account request", "--dry-run"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(!order.status.success());
    let stderr = String::from_utf8_lossy(&order.stderr);
    assert!(
        stderr.contains("more than 1000 catalog items match"),
        "{stderr}"
    );
}

#[tokio::test]
async fn catalog_order_resolves_a_name_when_later_pages_hold_no_other_match() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    forbid_order_post(&server).await;
    mount_catalog_search_page(&server, 0, aws_search_page(Some(AWS_ITEM), 99)).await;
    mount_catalog_search_page(&server, 100, aws_search_page(None, 0)).await;

    let order = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            "AWS account request",
            "--dry-run",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(
        order.status.success(),
        "{}",
        String::from_utf8_lossy(&order.stderr)
    );
}

#[tokio::test]
async fn catalog_order_refuses_unverifiable_date_formats_but_a_dry_run_only_warns() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let mut zoom = catalog_fixture("zoom_webinar");
    let zoom_id = zoom["result"]["sys_id"].as_str().unwrap().to_string();
    for variable in zoom["result"]["variables"].as_array_mut().unwrap() {
        if variable["name"] == "add_co_hosts" {
            variable["mandatory"] = false.into();
        }
    }
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{zoom_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(zoom))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/now/table/sys_user"))
        .and(query_param(
            "sysparm_query",
            "sys_id=javascript:gs.getUserID()",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"result": [{}]})))
        .mount(&server)
        .await;
    forbid_order_post(&server).await;
    let vars = [
        "--var",
        "topic=Launch",
        "--var",
        "select_webinar_start_time=2026-12-01 10:00:00",
        "--var",
        "select_webinar_end_time=2026-12-01 11:00:00",
    ];

    let dry_run = authenticated_command(&config_home, &server)
        .args(["catalog", "order", &zoom_id, "--dry-run", "--json"])
        .args(vars)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&dry_run.stderr);
    assert!(dry_run.status.success(), "{stderr}");
    assert!(
        stderr.contains("a real order needs --allow-unverified-formats"),
        "{stderr}"
    );

    let order = authenticated_command(&config_home, &server)
        .args(["catalog", "order", &zoom_id, "--yes", "--json"])
        .args(vars)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&order.stderr);
    assert!(!order.status.success(), "{stderr}");
    assert!(
        stderr.contains("pass --allow-unverified-formats to order anyway"),
        "{stderr}"
    );
}

#[tokio::test]
async fn catalog_order_needs_an_exact_name_but_show_accepts_a_single_match() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    forbid_order_post(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/sn_sc/servicecatalog/items"))
        .and(query_param("sysparm_text", "AWS"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "sys_id": AWS_ITEM,
                "name": "AWS account request",
                "sys_class_name": "sc_cat_item"
            }]
        })))
        .mount(&server)
        .await;

    let order = authenticated_command(&config_home, &server)
        .args(["catalog", "order", "AWS", "--dry-run"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(!order.status.success());
    let stderr = String::from_utf8_lossy(&order.stderr);
    assert!(stderr.contains("exact item name or sys_id"), "{stderr}");
    assert!(stderr.contains(AWS_ITEM), "{stderr}");

    let show = authenticated_command(&config_home, &server)
        .args(["catalog", "show", "AWS", "--json"])
        .output()
        .unwrap();
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    let item: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(item["result"]["sys_id"], AWS_ITEM);
    assert_eq!(item["result"]["orderable"], true);
}

#[test]
fn schema_marks_catalog_order_as_a_confirmed_dry_runnable_mutation() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .args(["schema", "--command", "catalog order"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(schema["behavior"]["mutation"], true);
    assert_eq!(schema["behavior"]["sideEffect"], "remote");
    assert_eq!(schema["behavior"]["requiresConfirmation"], true);
    assert_eq!(schema["behavior"]["supportsDryRun"], true);

    let search = command(&config_home)
        .args(["schema", "--command", "catalog search"])
        .output()
        .unwrap();
    let search: serde_json::Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search["behavior"]["mutation"], false);
}

#[tokio::test]
async fn catalog_request_vars_withholds_masked_values() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let zoom_item = "54c9ead947dec110f53d37d2846d43ea";
    let ritm_id = "0123456789abcdef0123456789abcdef";
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_req_item"))
        .and(query_param("sysparm_query", "number=RITM0010001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "sys_id": {"value": ritm_id, "display_value": ritm_id},
                "number": {"value": "RITM0010001", "display_value": "RITM0010001"},
                "cat_item": {"value": zoom_item, "display_value": "Zoom webinar"}
            }]
        })))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{zoom_item}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(catalog_fixture("zoom_webinar")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/sc_req_item/{ritm_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "variables.topic": {"value": "Quarterly review", "display_value": "Quarterly review"},
                "variables.descripion": {"value": "", "display_value": ""},
                "variables.requires_password": {"value": "true", "display_value": "true"},
                "variables.set_password": {"value": "hunter2-secret", "display_value": "hunter2-secret"}
            }
        })))
        .expect(2)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "requests", "vars", "RITM0010001", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let values: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        values,
        serde_json::json!({"topic": "Quarterly review", "requires_password": "true"})
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("set_password"), "{stderr}");
    assert!(!stderr.contains("hunter2"), "{stderr}");

    let text = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "requests",
            "vars",
            "RITM0010001",
            "--output",
            "text",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    assert!(stdout.contains("[REDACTED]"), "{stdout}");
    assert!(!stdout.contains("hunter2"), "{stdout}");
}

#[tokio::test]
async fn catalog_request_vars_leaves_read_only_values_out_of_the_reusable_export() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let zoom_item = "54c9ead947dec110f53d37d2846d43ea";
    let ritm_id = "0123456789abcdef0123456789abcdef";
    let mut zoom = catalog_fixture("zoom_webinar");
    for variable in zoom["result"]["variables"].as_array_mut().unwrap() {
        if variable["name"] == "topic" {
            variable["read_only"] = true.into();
        }
    }
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_req_item"))
        .and(query_param("sysparm_query", "number=RITM0010001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "sys_id": {"value": ritm_id, "display_value": ritm_id},
                "number": {"value": "RITM0010001", "display_value": "RITM0010001"},
                "cat_item": {"value": zoom_item, "display_value": "Zoom webinar"}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{zoom_item}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(zoom))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/sc_req_item/{ritm_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "variables.topic": {"value": "Quarterly review", "display_value": "Quarterly review"},
                "variables.descripion": {"value": "", "display_value": ""},
                "variables.requires_password": {"value": "true", "display_value": "true"},
                "variables.set_password": {"value": "hunter2-secret", "display_value": "hunter2-secret"}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "requests", "vars", "RITM0010001", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let values: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(values, serde_json::json!({"requires_password": "true"}));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("read-only variable(s) topic"), "{stderr}");
}

#[tokio::test]
async fn catalog_request_vars_export_reorders_like_the_original() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let zoom_item = "54c9ead947dec110f53d37d2846d43ea";
    let ritm_id = "0123456789abcdef0123456789abcdef";
    let mut zoom = catalog_fixture("zoom_webinar");
    for variable in zoom["result"]["variables"].as_array_mut().unwrap() {
        if variable["name"] == "descripion" {
            variable["value"] = "Agenda to follow".into();
        }
    }
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_req_item"))
        .and(query_param("sysparm_query", "number=RITM0010001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{
                "sys_id": {"value": ritm_id, "display_value": ritm_id},
                "number": {"value": "RITM0010001", "display_value": "RITM0010001"},
                "cat_item": {"value": zoom_item, "display_value": "Zoom webinar"}
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{zoom_item}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(zoom))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/now/table/sc_req_item/{ritm_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {
                "variables.topic": {"value": "Quarterly review", "display_value": "Quarterly review"},
                "variables.descripion": {"value": "", "display_value": ""},
                "variables.select_webinar_start_time": {
                    "value": "2027-03-01 18:00:00",
                    "display_value": "2027-03-01 10:00:00"
                },
                "variables.select_webinar_end_time": {"value": "", "display_value": ""}
            }
        })))
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "requests", "vars", "RITM0010001", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Omitting the cleared value would make a reorder submit the default,
    // and the stored UTC date-time would be read in the user's time zone.
    let values: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        values,
        serde_json::json!({
            "topic": "Quarterly review",
            "descripion": "",
            "select_webinar_start_time": "2027-03-01 10:00:00"
        })
    );
}

#[tokio::test]
async fn catalog_order_errors_never_echo_a_sensitive_value() {
    let all = order_with_echoed_secret(
        r#"{"topic": "Launch", "set_password": "s3cr3t-Pa55"}"#,
        ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {"message": "Invalid value s3cr3t-Pa55 for set_password", "detail": "s3cr3t-Pa55"},
            "status": "failure"
        })),
    )
    .await;
    assert!(!all.contains("s3cr3t-Pa55"), "{all}");
    assert!(
        all.contains("Invalid value [REDACTED] for set_password: [REDACTED]"),
        "{all}"
    );
}

/// A JSON `\u` escape of `hex`, built without writing one in this source.
fn u_escape(hex: &str) -> String {
    format!("{}u{hex}", '\\')
}

#[tokio::test]
async fn catalog_order_withholds_a_non_json_error_body_when_secrets_were_sent() {
    // The secret is Zq"Kx\Vé9=. A non-JSON error page can quote it in any
    // encoding (JSON-escaped, upper or lower hex, HTML-safe), so none of the
    // page is shown.
    let body = format!(
        r#"rejected "Zq\"Kx\\V{}9{}" and "Zq{}Kx{}V{}9{}" and "Zq\"Kx\\Vé9=""#,
        u_escape("00C9"),
        u_escape("003D"),
        u_escape("0022"),
        u_escape("005c"),
        u_escape("00e9"),
        u_escape("003d"),
    );
    let all = order_with_echoed_secret(
        r#"{"topic": "Launch", "set_password": "Zq\"Kx\\Vé9="}"#,
        ResponseTemplate::new(400).set_body_raw(body, "text/plain"),
    )
    .await;
    assert!(!all.contains("Zq"), "{all}");
    assert!(!all.contains("Kx"), "{all}");
    assert!(
        all.contains("HTTP 400 (response body withheld: the order contains sensitive values)"),
        "{all}"
    );
}

#[tokio::test]
async fn catalog_order_withholds_an_error_message_quoting_escaped_json() {
    // A JSON error message can itself quote the request as JSON text, so
    // after decoding the response the secret is still escaped.
    let message = format!(
        r#"bad request {{"set_password":"Zq\"Kx\\V{}9{}"}}"#,
        u_escape("00C9"),
        u_escape("003D"),
    );
    let all = order_with_echoed_secret(
        r#"{"topic": "Launch", "set_password": "Zq\"Kx\\Vé9="}"#,
        ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {"message": message},
            "status": "failure"
        })),
    )
    .await;
    assert!(!all.contains("Zq"), "{all}");
    assert!(!all.contains("Kx"), "{all}");
    assert!(all.contains("withheld"), "{all}");
}

#[tokio::test]
async fn catalog_order_withholds_an_error_message_quoting_html_or_url_encoding() {
    // The secret is P&ssword=1. HTML and URL encoding change it without a
    // backslash, so a message using either is withheld; a raw echo is still
    // shown with the value replaced.
    for (message, shown) in [
        ("Invalid value P&amp;ssword=1", false),
        ("Invalid value P%26ssword%3D1", false),
        ("Invalid value P&ssword=1", true),
    ] {
        let all = order_with_echoed_secret(
            r#"{"topic": "Launch", "set_password": "P&ssword=1"}"#,
            ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": {"message": message},
                "status": "failure"
            })),
        )
        .await;
        assert!(!all.contains("ssword"), "{message}: {all}");
        if shown {
            assert!(all.contains("Invalid value [REDACTED]"), "{message}: {all}");
        } else {
            assert!(all.contains("error message withheld"), "{message}: {all}");
        }
    }
}

#[tokio::test]
async fn catalog_order_errors_never_echo_part_of_a_sensitive_value() {
    // A long non-JSON error page is shortened for display; a secret quoted
    // across the cut must not survive as a prefix.
    let body = format!("{}s3cr3tPa55w0rdTail and more", "x".repeat(490));
    let all = order_with_echoed_secret(
        r#"{"topic": "Launch", "set_password": "s3cr3tPa55w0rdTail"}"#,
        ResponseTemplate::new(502).set_body_raw(body, "text/html"),
    )
    .await;
    assert!(!all.contains("s3cr3"), "{all}");
    assert!(all.contains("response body withheld"), "{all}");
}

#[tokio::test]
async fn catalog_order_without_secrets_shows_a_non_json_error_body() {
    let all = order_with_echoed_secret(
        r#"{"topic": "Launch"}"#,
        ResponseTemplate::new(502).set_body_raw("upstream gateway down", "text/html"),
    )
    .await;
    assert!(all.contains("upstream gateway down"), "{all}");
    assert!(!all.contains("withheld"), "{all}");
}

/// Orders the Zoom fixture (all variables optional) with the given variables
/// on stdin against an order endpoint answering `error`, and returns stdout
/// and stderr together.
async fn order_with_echoed_secret(vars: &str, error: ResponseTemplate) -> String {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let mut zoom = catalog_fixture("zoom_webinar");
    fn optional(node: &mut serde_json::Value) {
        match node {
            serde_json::Value::Object(map) => {
                if map.contains_key("mandatory") {
                    map.insert("mandatory".into(), false.into());
                }
                map.values_mut().for_each(optional);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(optional),
            _ => {}
        }
    }
    optional(&mut zoom);
    let zoom_id = zoom["result"]["sys_id"].as_str().unwrap().to_string();
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{zoom_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(zoom))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{zoom_id}/order_now"
        )))
        .respond_with(error)
        .expect(1)
        .mount(&server)
        .await;
    let mut child = authenticated_command(&config_home, &server)
        .args(["catalog", "order", &zoom_id, "--yes", "--vars-file", "-"])
        .args(["--output", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(vars.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const CART_URL: &str = "/api/sn_sc/servicecatalog/cart";
const CART_ITEM_ID: &str = "346489831b181710b97aa975624bcb75";
const OTHER_CART_ITEM_ID: &str = "456489831b181710b97aa975624bcbb2";
const CART_REQUEST_ID: &str = "c54ae46e1b181310b97aa975624bcbf3";

fn cart_item(cart_item_id: &str, name: &str) -> serde_json::Value {
    serde_json::json!({
        "cart_item_id": cart_item_id,
        "item_name": name,
        "quantity": "1",
        "can_order": true,
        "catalog_item_id": AWS_ITEM,
    })
}

fn cart_answer(items: Vec<serde_json::Value>) -> serde_json::Value {
    let mut cart = serde_json::json!({ "cart_id": CART_ID, "subtotal_price": "-" });
    if !items.is_empty() {
        cart["none"] = serde_json::json!({ "frequency_label": null, "items": items });
    }
    serde_json::json!({ "result": cart })
}

/// Answers the first cart read with `first` and every later one with `later`,
/// as when the cart changes between two reads.
async fn mount_changing_cart(
    server: &MockServer,
    first: Vec<serde_json::Value>,
    later: Vec<serde_json::Value>,
) {
    Mock::given(method("GET"))
        .and(path(CART_URL))
        .respond_with(ResponseTemplate::new(200).set_body_json(cart_answer(first)))
        .up_to_n_times(1)
        .mount(server)
        .await;
    mount_cart(server, later).await;
}

async fn mount_cart(server: &MockServer, items: Vec<serde_json::Value>) {
    Mock::given(method("GET"))
        .and(path(CART_URL))
        .respond_with(ResponseTemplate::new(200).set_body_json(cart_answer(items)))
        .mount(server)
        .await;
}

/// Fails the test on any request that could change the instance.
async fn forbid_writes(server: &MockServer) {
    for verb in ["POST", "PUT", "PATCH", "DELETE"] {
        Mock::given(method(verb))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(server)
            .await;
    }
}

async fn mount_submit_order(server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(format!("{CART_URL}/submit_order")))
        .respond_with(response)
        .expect(1)
        .mount(server)
        .await;
}

fn cart_request_answer() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "result": {
            "request_id": CART_REQUEST_ID,
            "request_number": "REQ0010002",
            "table": "sc_request"
        }
    }))
}

async fn mount_cart_request_items(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/now/table/sc_req_item"))
        .and(query_param(
            "sysparm_query",
            format!("request={CART_REQUEST_ID}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": [{"number": "RITM0010002", "sys_id": "1123456789abcdef0123456789abcdef"}]
        })))
        .expect(1)
        .mount(server)
        .await;
}

fn json_error(output: &std::process::Output) -> serde_json::Value {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr
        .lines()
        .rev()
        .find(|line| line.starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON error on stderr: {stderr}"));
    serde_json::from_str(line).unwrap()
}

#[tokio::test]
async fn catalog_cart_show_lists_items_from_every_frequency_group() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let mut answer = cart_answer(vec![cart_item(CART_ITEM_ID, "AWS account request")]);
    answer["result"]["monthly"] = serde_json::json!({
        "frequency_label": "Monthly",
        "items": [cart_item(OTHER_CART_ITEM_ID, "Zoom")]
    });
    Mock::given(method("GET"))
        .and(path(CART_URL))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .mount(&server)
        .await;
    forbid_writes(&server).await;

    let output = authenticated_command(&config_home, &server)
        .env("SERVICENOW_READ_ONLY", "true")
        .args(["catalog", "cart", "show", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cart: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cart["cart_id"], CART_ID);
    let items = cart["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{cart}");
    let ids: std::collections::BTreeSet<&str> = items
        .iter()
        .map(|item| item["cart_item_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [CART_ITEM_ID, OTHER_CART_ITEM_ID].into_iter().collect()
    );
    let zoom = items
        .iter()
        .find(|item| item["item_name"] == "Zoom")
        .unwrap();
    assert_eq!(zoom["frequency"], "monthly");

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "show", "--output", "text"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 item(s)"), "{stdout}");
    assert!(
        stdout.contains("AWS account request") && stdout.contains(OTHER_CART_ITEM_ID),
        "{stdout}"
    );
}

#[tokio::test]
async fn catalog_cart_show_reports_an_empty_cart() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_cart(&server, vec![]).await;
    forbid_writes(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "show", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let cart: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cart, serde_json::json!({"cart_id": CART_ID, "items": []}));

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "show", "--output", "text"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Your cart is empty."
    );
}

#[tokio::test]
async fn catalog_cart_show_refuses_an_answer_without_a_cart_id() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(CART_URL))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"result": {"items": []}})),
        )
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "show", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("without a cart id"));
}

#[tokio::test]
async fn catalog_cart_submit_orders_the_cart_and_reports_the_requested_items() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_cart(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
    )
    .await;
    mount_submit_order(&server, cart_request_answer()).await;
    mount_cart_request_items(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "submit", "--yes", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["request"]["request_number"], "REQ0010002");
    assert_eq!(result["requestedItems"][0]["number"], "RITM0010002");
}

#[tokio::test]
async fn catalog_cart_submit_refuses_an_empty_cart_without_posting() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_cart(&server, vec![]).await;
    forbid_writes(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "submit", "--yes", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cart is empty"));
}

#[tokio::test]
async fn catalog_cart_writes_are_blocked_by_read_only_mode_before_network_access() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    for args in [
        vec!["catalog", "cart", "submit", "--yes"],
        vec!["catalog", "cart", "remove", CART_ITEM_ID, "--yes"],
        vec!["catalog", "cart", "clear", "--yes"],
    ] {
        let output = authenticated_command(&config_home, &server)
            .env("SERVICENOW_READ_ONLY", "true")
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("read-only"),
            "{args:?}"
        );
    }
}

#[tokio::test]
async fn catalog_cart_writes_without_yes_refuse_a_non_interactive_stdin() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_cart(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
    )
    .await;
    forbid_writes(&server).await;

    for (args, refusal) in [
        (
            vec!["catalog", "cart", "submit"],
            "cart submission cancelled",
        ),
        (
            vec!["catalog", "cart", "remove", CART_ITEM_ID],
            "cart item removal cancelled",
        ),
        (vec!["catalog", "cart", "clear"], "cart clearing cancelled"),
    ] {
        let output = authenticated_command(&config_home, &server)
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(refusal), "{args:?}: {stderr}");
    }
}

#[tokio::test]
async fn catalog_cart_submit_that_answers_with_a_cart_never_suggests_resubmitting() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_cart(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
    )
    .await;
    mount_submit_order(
        &server,
        ResponseTemplate::new(200)
            .set_body_json(serde_json::json!({"result": {"cart_id": CART_ID}})),
    )
    .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "submit", "--yes", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = json_error(&output);
    assert_ne!(error["error"]["kind"], "not_ordered", "{error}");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("unclear whether anything was ordered"),
        "{message}"
    );
    assert!(message.contains("catalog requests mine"), "{message}");
    assert!(!error.to_string().contains("cart submit`"), "{error}");
}

#[tokio::test]
async fn catalog_cart_remove_deletes_only_an_item_in_the_cart() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_cart(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
    )
    .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{CART_URL}/{CART_ITEM_ID}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "cart",
            "remove",
            &format!(" {} ", CART_ITEM_ID.to_ascii_uppercase()),
            "--yes",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result,
        serde_json::json!({
            "removed": true,
            "cart_id": CART_ID,
            "cart_item_id": CART_ITEM_ID,
            "item_name": "AWS account request",
        })
    );
}

#[tokio::test]
async fn catalog_cart_remove_of_an_item_not_in_the_cart_is_not_found_without_deleting() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_cart(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
    )
    .await;
    forbid_writes(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "remove", OTHER_CART_ITEM_ID, "--yes"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not in your cart"));

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "remove", "not-a-sys-id", "--yes"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[tokio::test]
async fn catalog_cart_remove_reports_an_item_the_instance_no_longer_has_as_not_found() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_cart(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
    )
    .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{CART_URL}/{CART_ITEM_ID}")))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {"message": "Invalid Cart Item ID!", "detail": ""},
            "status": "failure"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "remove", CART_ITEM_ID, "--yes"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid Cart Item ID"));
}

#[tokio::test]
async fn catalog_cart_clear_empties_a_cart_that_holds_items() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_cart(
        &server,
        vec![
            cart_item(CART_ITEM_ID, "AWS account request"),
            cart_item(OTHER_CART_ITEM_ID, "Zoom"),
        ],
    )
    .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{CART_URL}/{CART_ID}/empty")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "clear", "--yes", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result,
        serde_json::json!({"cleared": true, "cart_id": CART_ID, "items_removed": 2})
    );
}

#[tokio::test]
async fn catalog_cart_clear_of_an_empty_cart_deletes_nothing_and_needs_no_confirmation() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_cart(&server, vec![]).await;
    forbid_writes(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "clear", "--output", "text"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Your cart is already empty."
    );
}

fn add_to_cart_url() -> String {
    format!("/api/sn_sc/servicecatalog/items/{AWS_ITEM}/add_to_cart")
}

/// Mounts an empty cart for the `--checkout` preflight and an `add_to_cart`
/// answer describing the cart afterwards.
async fn mount_checkout_add(server: &MockServer, items_after: Vec<serde_json::Value>) {
    mount_aws_item(server).await;
    mount_changing_cart(server, vec![], items_after.clone()).await;
    let mut answer = cart_answer(items_after.clone());
    answer["result"]["items"] = serde_json::Value::Array(items_after);
    answer["result"].as_object_mut().unwrap().remove("none");
    Mock::given(method("POST"))
        .and(path(add_to_cart_url()))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "sysparm_quantity": "1",
            "variables": {
                "division": "IT",
                "environment": "Dev",
                "describe_briefly_the_purpose_of_this_account": "Sandbox"
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn catalog_order_checkout_adds_the_item_and_submits_the_cart() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_checkout_add(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
    )
    .await;
    mount_submit_order(&server, cart_request_answer()).await;
    mount_cart_request_items(&server).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{AWS_ITEM}/order_now"
        )))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--yes",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["request"]["request_number"], "REQ0010002");
    assert_eq!(result["requestedItems"][0]["number"], "RITM0010002");
}

#[tokio::test]
async fn catalog_order_checkout_refuses_a_cart_that_already_holds_items() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    mount_cart(&server, vec![cart_item(OTHER_CART_ITEM_ID, "Zoom")]).await;
    forbid_writes(&server).await;

    for extra in [vec!["--yes"], vec!["--dry-run"]] {
        let output = authenticated_command(&config_home, &server)
            .args(["catalog", "order", AWS_ITEM, "--checkout", "--json"])
            .args(&extra)
            .args(AWS_VARS)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7), "{extra:?}");
        assert!(output.stdout.is_empty(), "{extra:?}");
        let error = json_error(&output);
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains("Zoom"), "{message}");
        assert!(message.contains("catalog cart clear"), "{message}");
    }
}

#[tokio::test]
async fn catalog_order_checkout_dry_run_marks_the_plan_without_writing() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    mount_cart(&server, vec![]).await;
    forbid_writes(&server).await;

    let output = authenticated_command(&config_home, &server)
        .env("SERVICENOW_READ_ONLY", "true")
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--dry-run",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["dryRun"], true);
    assert_eq!(plan["checkout"], true);
    assert_eq!(plan["variables"]["division"], "IT");
}

#[tokio::test]
async fn catalog_order_checkout_refuses_to_order_for_someone_else_before_network_access() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--requested-for",
            "abel.tuter",
            "--yes",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--requested-for"));
}

#[tokio::test]
async fn catalog_order_checkout_does_not_submit_a_cart_that_gained_other_items() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_checkout_add(
        &server,
        vec![
            cart_item(CART_ITEM_ID, "AWS account request"),
            cart_item(OTHER_CART_ITEM_ID, "Zoom"),
        ],
    )
    .await;
    Mock::given(method("POST"))
        .and(path(format!("{CART_URL}/submit_order")))
        .respond_with(cart_request_answer())
        .expect(0)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--yes",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(8));
    let error = json_error(&output);
    assert_eq!(error["error"]["kind"], "not_ordered", "{error}");
    assert_eq!(error["error"]["cart"]["sys_id"], CART_ID, "{error}");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("2 item(s)"), "{message}");
    assert!(message.contains("still in your cart"), "{message}");
}

#[tokio::test]
async fn catalog_order_checkout_with_a_refused_submission_leaves_the_item_in_the_cart() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_checkout_add(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
    )
    .await;
    mount_submit_order(
        &server,
        ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {"message": "Cart is empty!", "detail": ""},
            "status": "failure"
        })),
    )
    .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--yes",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(8));
    let error = json_error(&output);
    assert_eq!(error["error"]["kind"], "not_ordered", "{error}");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("submitting the cart failed"), "{message}");
    assert!(message.contains("Cart is empty!"), "{message}");
}

#[tokio::test]
async fn catalog_order_checkout_with_a_server_error_on_submission_may_have_ordered() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_checkout_add(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
    )
    .await;
    mount_submit_order(
        &server,
        ResponseTemplate::new(503).set_body_json(serde_json::json!({
            "error": {"message": "Service unavailable", "detail": ""},
            "status": "failure"
        })),
    )
    .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--yes",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_ne!(
        output.status.code(),
        Some(8),
        "a server error may have ordered"
    );
    let error = json_error(&output);
    assert_ne!(error["error"]["kind"], "not_ordered", "{error}");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("may have"), "{message}");
}

#[tokio::test]
async fn catalog_order_checkout_is_blocked_by_read_only_mode_before_reading_the_cart() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    Mock::given(method("GET"))
        .and(path(CART_URL))
        .respond_with(ResponseTemplate::new(200).set_body_json(cart_answer(vec![])))
        .expect(0)
        .mount(&server)
        .await;
    forbid_writes(&server).await;

    let output = authenticated_command(&config_home, &server)
        .env("SERVICENOW_READ_ONLY", "true")
        .args(["catalog", "order", AWS_ITEM, "--checkout", "--yes"])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("read-only"));
}

/// Mounts the Zoom item with every variable optional and returns its id.
async fn mount_optional_zoom(server: &MockServer) -> String {
    let zoom = catalog_fixture("zoom_webinar");
    let zoom_id = zoom["result"]["sys_id"].as_str().unwrap().to_string();
    let mut optional = zoom.clone();
    fn relax(node: &mut serde_json::Value) {
        match node {
            serde_json::Value::Object(map) => {
                if map.contains_key("mandatory") {
                    map.insert("mandatory".into(), false.into());
                }
                map.values_mut().for_each(relax);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(relax),
            _ => {}
        }
    }
    relax(&mut optional);
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{zoom_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(optional))
        .mount(server)
        .await;
    zoom_id
}

fn zoom_in_cart(zoom_id: &str) -> serde_json::Value {
    let mut item = cart_item(CART_ITEM_ID, "Zoom");
    item["catalog_item_id"] = zoom_id.into();
    item
}

fn refusal_quoting_the_secret() -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(serde_json::json!({
        "error": {"message": "Invalid value s3cr3t-Pa55 for set_password", "detail": "s3cr3t-Pa55"},
        "status": "failure"
    }))
}

/// Orders Zoom with `--checkout`, passing its password variable on stdin,
/// and returns the output with stdout and stderr joined.
fn checkout_zoom(
    config_home: &TempDir,
    server: &MockServer,
    zoom_id: &str,
) -> (Option<i32>, String) {
    let mut child = authenticated_command(config_home, server)
        .args(["catalog", "order", zoom_id, "--checkout", "--yes"])
        .args(["--vars-file", "-", "--output", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"topic": "Launch", "set_password": "s3cr3t-Pa55"}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code(), all)
}

#[tokio::test]
async fn catalog_order_checkout_never_echoes_a_sensitive_value_from_a_failed_submission() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let zoom_id = mount_optional_zoom(&server).await;
    mount_changing_cart(&server, vec![], vec![zoom_in_cart(&zoom_id)]).await;
    let mut added = cart_answer(vec![]);
    added["result"]["items"] = serde_json::json!([zoom_in_cart(&zoom_id)]);
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{zoom_id}/add_to_cart"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(added))
        .expect(1)
        .mount(&server)
        .await;
    mount_submit_order(&server, refusal_quoting_the_secret()).await;

    let (code, all) = checkout_zoom(&config_home, &server, &zoom_id);
    assert_eq!(code, Some(8), "{all}");
    assert!(!all.contains("s3cr3t-Pa55"), "{all}");
    assert!(all.contains("[REDACTED]"), "{all}");
}

#[tokio::test]
async fn catalog_order_checkout_never_echoes_a_sensitive_value_from_a_refused_add() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let zoom_id = mount_optional_zoom(&server).await;
    mount_cart(&server, vec![]).await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/api/sn_sc/servicecatalog/items/{zoom_id}/add_to_cart"
        )))
        .respond_with(refusal_quoting_the_secret())
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{CART_URL}/submit_order")))
        .respond_with(cart_request_answer())
        .expect(0)
        .mount(&server)
        .await;

    let (code, all) = checkout_zoom(&config_home, &server, &zoom_id);
    assert_eq!(code, Some(5), "{all}");
    assert!(!all.contains("s3cr3t-Pa55"), "{all}");
    assert!(all.contains("[REDACTED]"), "{all}");
}

#[tokio::test]
async fn catalog_order_checkout_that_lists_no_added_item_stops_without_submitting() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_checkout_add(&server, vec![]).await;
    Mock::given(method("POST"))
        .and(path(format!("{CART_URL}/submit_order")))
        .respond_with(cart_request_answer())
        .expect(0)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--yes",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error = json_error(&output);
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains("listed no items"), "{message}");
    assert!(message.contains("nothing was ordered"), "{message}");
}

#[tokio::test]
async fn catalog_order_checkout_never_submits_a_cart_holding_a_different_item() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let mut other = cart_item(OTHER_CART_ITEM_ID, "Zoom");
    other["catalog_item_id"] = "54c9ead947dec110f53d37d2846d43ea".into();
    mount_checkout_add(&server, vec![other]).await;
    Mock::given(method("POST"))
        .and(path(format!("{CART_URL}/submit_order")))
        .respond_with(cart_request_answer())
        .expect(0)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--yes",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(8));
    let message = json_error(&output)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("\"Zoom\" instead"), "{message}");
}

/// Mounts a cart holding `items` and a submission refused with a message
/// that quotes a secret.
async fn mount_cart_refusing_with_a_secret(server: &MockServer, items: Vec<serde_json::Value>) {
    mount_cart(server, items).await;
    mount_submit_order(
        server,
        ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {"message": "Invalid value s3cr3t-Pa55 for set_password"},
            "status": "failure"
        })),
    )
    .await;
}

fn submit_cart(config_home: &TempDir, server: &MockServer) -> std::process::Output {
    authenticated_command(config_home, server)
        .args(["catalog", "cart", "submit", "--yes", "--json"])
        .output()
        .unwrap()
}

#[tokio::test]
async fn catalog_cart_submit_withholds_the_error_for_a_cart_with_a_sensitive_item() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let zoom = catalog_fixture("zoom_webinar");
    let zoom_id = zoom["result"]["sys_id"].as_str().unwrap().to_string();
    Mock::given(method("GET"))
        .and(path(format!("/api/sn_sc/servicecatalog/items/{zoom_id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(zoom))
        .expect(1)
        .mount(&server)
        .await;
    let mut zoom_in_cart = cart_item(CART_ITEM_ID, "Zoom");
    zoom_in_cart["catalog_item_id"] = zoom_id.to_uppercase().into();
    mount_cart_refusing_with_a_secret(&server, vec![zoom_in_cart]).await;

    let output = submit_cart(&config_home, &server);
    assert_eq!(output.status.code(), Some(5));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("s3cr3t-Pa55"), "{stderr}");
    assert!(stderr.contains("error message withheld"), "{stderr}");
}

#[tokio::test]
async fn catalog_cart_submit_withholds_the_error_when_an_item_cannot_be_checked() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let mut unknown = cart_item(CART_ITEM_ID, "AWS account request");
    unknown.as_object_mut().unwrap().remove("catalog_item_id");
    mount_cart_refusing_with_a_secret(&server, vec![unknown]).await;

    let output = submit_cart(&config_home, &server);
    assert_eq!(output.status.code(), Some(5));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("s3cr3t-Pa55"), "{stderr}");
    assert!(stderr.contains("error message withheld"), "{stderr}");
}

#[tokio::test]
async fn catalog_cart_submit_shows_the_error_for_a_cart_without_sensitive_items() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    mount_cart_refusing_with_a_secret(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
    )
    .await;

    let output = submit_cart(&config_home, &server);
    assert_eq!(output.status.code(), Some(5));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Invalid value"), "{stderr}");
    assert!(!stderr.contains("error message withheld"), "{stderr}");
}

#[tokio::test]
async fn catalog_order_checkout_whose_item_left_the_cart_after_a_refusal_may_have_ordered() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    // The add answer lists the item, but by the time the refusal is checked
    // the cart is empty again: the portal ordered or removed it.
    mount_aws_item(&server).await;
    mount_cart(&server, vec![]).await;
    let added = serde_json::json!({"result": {
        "cart_id": CART_ID,
        "items": [cart_item(CART_ITEM_ID, "AWS account request")],
    }});
    Mock::given(method("POST"))
        .and(path(add_to_cart_url()))
        .respond_with(ResponseTemplate::new(200).set_body_json(added))
        .expect(1)
        .mount(&server)
        .await;
    mount_submit_order(
        &server,
        ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {"message": "Cart is empty!"}, "status": "failure"
        })),
    )
    .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--yes",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let message = json_error(&output)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("may have been ordered"), "{message}");
    assert!(message.contains("catalog requests mine"), "{message}");
}

#[tokio::test]
async fn catalog_cart_submit_refuses_a_cart_that_changed_after_it_was_read() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    mount_changing_cart(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
        vec![
            cart_item(CART_ITEM_ID, "AWS account request"),
            cart_item(OTHER_CART_ITEM_ID, "Added in the portal"),
        ],
    )
    .await;
    forbid_writes(&server).await;

    let output = submit_cart(&config_home, &server);
    assert_eq!(output.status.code(), Some(7));
    let message = json_error(&output)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("Added in the portal"), "{message}");
    assert!(message.contains("nothing was changed"), "{message}");
}

#[tokio::test]
async fn catalog_cart_submit_refuses_a_cart_whose_quantity_changed() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    let mut more = cart_item(CART_ITEM_ID, "AWS account request");
    more["quantity"] = "3".into();
    mount_changing_cart(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
        vec![more],
    )
    .await;
    forbid_writes(&server).await;

    let output = submit_cart(&config_home, &server);
    assert_eq!(output.status.code(), Some(7));
}

#[tokio::test]
async fn catalog_cart_clear_refuses_a_cart_that_changed_after_it_was_read() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_changing_cart(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
        vec![
            cart_item(CART_ITEM_ID, "AWS account request"),
            cart_item(OTHER_CART_ITEM_ID, "Added in the portal"),
        ],
    )
    .await;
    forbid_writes(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "cart", "clear", "--yes", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
}

#[tokio::test]
async fn catalog_cart_submit_withholds_the_error_for_a_cart_with_a_multi_row_set() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item_with_multi_row_set(&server, false).await;
    mount_cart_refusing_with_a_secret(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request")],
    )
    .await;

    let output = submit_cart(&config_home, &server);
    assert_eq!(output.status.code(), Some(5));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("s3cr3t-Pa55"), "{stderr}");
    assert!(stderr.contains("error message withheld"), "{stderr}");
}

#[tokio::test]
async fn catalog_cart_submit_withholds_the_error_when_an_item_definition_cannot_be_read() {
    for status in [404, 500] {
        let config_home = TempDir::new().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/api/sn_sc/servicecatalog/items/{AWS_ITEM}")))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        mount_cart_refusing_with_a_secret(
            &server,
            vec![cart_item(CART_ITEM_ID, "AWS account request")],
        )
        .await;

        let output = submit_cart(&config_home, &server);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(5), "{status}: {stderr}");
        assert!(!stderr.contains("s3cr3t-Pa55"), "{status}: {stderr}");
        assert!(
            stderr.contains("error message withheld"),
            "{status}: {stderr}"
        );
    }
}

#[tokio::test]
async fn catalog_cart_submit_withholds_the_error_when_any_one_item_is_sensitive() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    let zoom_id = mount_optional_zoom(&server).await;
    let mut zoom = zoom_in_cart(&zoom_id);
    zoom["cart_item_id"] = OTHER_CART_ITEM_ID.into();
    mount_cart_refusing_with_a_secret(
        &server,
        vec![cart_item(CART_ITEM_ID, "AWS account request"), zoom],
    )
    .await;

    let output = submit_cart(&config_home, &server);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(5), "{stderr}");
    assert!(!stderr.contains("s3cr3t-Pa55"), "{stderr}");
    assert!(stderr.contains("error message withheld"), "{stderr}");
}

#[tokio::test]
async fn catalog_order_checkout_asks_before_adding_anything() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    mount_cart(&server, vec![]).await;
    forbid_writes(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args(["catalog", "order", AWS_ITEM, "--checkout"])
        .args(AWS_VARS)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cancelled"));
}

#[tokio::test]
async fn catalog_order_checkout_matches_the_added_item_in_any_case() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let mut added = cart_item(CART_ITEM_ID, "AWS account request");
    added["catalog_item_id"] = AWS_ITEM.to_uppercase().into();
    mount_checkout_add(&server, vec![added]).await;
    mount_submit_order(&server, cart_request_answer()).await;
    mount_cart_request_items(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            &AWS_ITEM.to_uppercase(),
            "--checkout",
            "--yes",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn catalog_order_checkout_submits_a_lone_added_item_the_answer_does_not_identify() {
    // The cart was empty before the add, so a single item in the answer is
    // the added one even when the instance omits its catalog item id.
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    let mut added = cart_item(CART_ITEM_ID, "AWS account request");
    added.as_object_mut().unwrap().remove("catalog_item_id");
    mount_checkout_add(&server, vec![added]).await;
    mount_submit_order(&server, cart_request_answer()).await;
    mount_cart_request_items(&server).await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--yes",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn catalog_order_checkout_with_an_unreadable_cart_after_a_refusal_may_have_ordered() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    Mock::given(method("GET"))
        .and(path(CART_URL))
        .respond_with(ResponseTemplate::new(200).set_body_json(cart_answer(vec![])))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(CART_URL))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let added = serde_json::json!({"result": {
        "cart_id": CART_ID,
        "items": [cart_item(CART_ITEM_ID, "AWS account request")],
    }});
    Mock::given(method("POST"))
        .and(path(add_to_cart_url()))
        .respond_with(ResponseTemplate::new(200).set_body_json(added))
        .expect(1)
        .mount(&server)
        .await;
    mount_submit_order(
        &server,
        ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": {"message": "Cart is empty!"}, "status": "failure"
        })),
    )
    .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--yes",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let message = json_error(&output)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("may have been ordered"), "{message}");
}

#[tokio::test]
async fn catalog_order_checkout_with_an_unreadable_add_answer_stops_without_submitting() {
    let config_home = TempDir::new().unwrap();
    let server = MockServer::start().await;
    mount_aws_item(&server).await;
    mount_cart(&server, vec![]).await;
    Mock::given(method("POST"))
        .and(path(add_to_cart_url()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"result": {"items": []}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{CART_URL}/submit_order")))
        .respond_with(cart_request_answer())
        .expect(0)
        .mount(&server)
        .await;

    let output = authenticated_command(&config_home, &server)
        .args([
            "catalog",
            "order",
            AWS_ITEM,
            "--checkout",
            "--yes",
            "--json",
        ])
        .args(AWS_VARS)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let message = json_error(&output)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("could not be read"), "{message}");
    assert!(message.contains("nothing was ordered"), "{message}");
}
