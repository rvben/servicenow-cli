use std::process::Command;

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
        .env_remove("SERVICENOW_READ_ONLY");
    command
}

fn authenticated_command(config_home: &TempDir, server: &MockServer) -> Command {
    let mut command = command(config_home);
    command
        .env("SERVICENOW_INSTANCE", server.uri())
        .env("SERVICENOW_USERNAME", "admin")
        .env("SERVICENOW_PASSWORD", "secret");
    command
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
fn setup_schema_describes_adaptive_onboarding_defaults() {
    let config_home = TempDir::new().unwrap();
    let output = command(&config_home)
        .args(["schema", "--command", "setup"])
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
async fn setup_can_fall_back_to_the_protected_config_file() {
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

    let mut setup = assert_cmd::Command::from_std(command(&config_home));
    let output = setup
        .args([
            "setup",
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
        .args(["auth", "logout", "work"])
        .assert()
        .success();
    let config = std::fs::read_to_string(config_path).unwrap();
    assert!(!config.contains("password = \"secret\""));
}

#[tokio::test]
async fn setup_detects_microsoft_entra_and_selects_zero_admin_browser_sign_in() {
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
            "setup",
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
async fn setup_does_not_treat_a_local_login_form_as_basic_authentication() {
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
        .args(["setup", "work", "--instance", &server.uri()])
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

    let mut setup = assert_cmd::Command::from_std(command(&config_home));
    setup
        .args([
            "setup",
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
