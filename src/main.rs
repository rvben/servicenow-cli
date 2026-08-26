use std::io::{IsTerminal, Read, Write};

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use dialoguer::{Confirm, Editor, Input, Password, Select};
use serde_json::{Map, Value};
use servicenow_cli::api::{ApiError, DisplayValue, ListOptions, ServiceNowClient};
use servicenow_cli::attachment;
use servicenow_cli::commands::{
    INCIDENT_HUMAN_FIELDS, INCIDENT_LIST_FIELDS, build_body, parse_fields, print_record,
    print_records, print_records_or, record_sys_id,
};
use servicenow_cli::config::{
    AuthType, Config, ProfileConfig, active_profile_name, config_path, delete_stored_credential,
    init_document, profile_defaults, profile_summaries, remove_profile, save_profile, use_profile,
};
use servicenow_cli::credentials::{self, StoredCredential};
use servicenow_cli::incident;
use servicenow_cli::metadata::{self, ReferenceKind};
use servicenow_cli::output::{OutputConfig, OutputFormat, exit_code, print_error};
use servicenow_cli::record;

#[derive(Parser)]
#[command(
    name = "servicenow",
    version,
    about = "Fast, safe, human-friendly ServiceNow operations from the terminal",
    arg_required_else_help = true,
    after_help = "Get started:\n  servicenow init                    Configure and sign in\n  servicenow doctor                  Check configuration and instance access\n  servicenow incidents mine          See what needs your attention\n  servicenow schema --command 'incidents list'\n                                      Inspect one command for automation"
)]
struct Cli {
    /// Instance name, hostname, or URL
    #[arg(long, env = "SERVICENOW_INSTANCE")]
    instance: Option<String>,

    /// Username for Basic authentication
    #[arg(long, env = "SERVICENOW_USERNAME")]
    username: Option<String>,

    /// Config profile
    #[arg(long, env = "SERVICENOW_PROFILE")]
    profile: Option<String>,

    /// Output format: auto, table, text, json, jsonl, yaml, or csv
    #[arg(short, long, global = true, default_value = "auto")]
    output: String,

    /// Output JSON (alias for --output json)
    #[arg(long, global = true, hide = true)]
    json: bool,

    /// Suppress non-data messages
    #[arg(long, global = true)]
    quiet: bool,

    /// Show secret-free browser sign-in progress on stderr
    #[arg(
        short,
        long,
        global = true,
        env = "SERVICENOW_VERBOSE",
        conflicts_with = "quiet"
    )]
    verbose: bool,

    /// Disable ANSI color even when stdout is a terminal
    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Connect an instance with guided, secure setup
    Init {
        /// Profile name to create or replace
        #[arg(default_value = "default")]
        profile: String,

        /// Instance name, hostname, or URL
        #[arg(long)]
        instance: Option<String>,

        /// Authentication method (auto-detected when omitted)
        #[arg(long, value_enum)]
        method: Option<AuthType>,

        /// Username for Basic authentication
        #[arg(long)]
        username: Option<String>,

        /// OAuth application client ID
        #[arg(long)]
        client_id: Option<String>,

        /// Space-separated OAuth scopes
        #[arg(long)]
        scope: Option<String>,

        /// Registered loopback OAuth redirect URI
        #[arg(long)]
        redirect_uri: Option<String>,

        /// Read the password/token/client secret from stdin
        #[arg(long)]
        secret_stdin: bool,

        /// Store the credential in the protected config file instead of the OS keychain
        #[arg(long)]
        insecure_storage: bool,

        /// Do not open a browser automatically (OAuth only)
        #[arg(long)]
        no_browser: bool,

        /// Block all writes when this profile is active
        #[arg(long)]
        read_only: bool,
    },

    /// Sign in securely and inspect authentication
    #[command(subcommand)]
    Auth(AuthCommand),

    /// Manage named instance profiles
    #[command(subcommand, visible_alias = "profiles")]
    Profile(ProfileCommand),

    /// Work with incidents
    #[command(subcommand, visible_alias = "incident")]
    Incidents(IncidentsCommand),

    /// List, upload, download, and delete record attachments
    #[command(subcommand, visible_alias = "attachment")]
    Attachments(AttachmentsCommand),

    /// Perform generic CRUD operations through the Table API
    #[command(subcommand, visible_alias = "table")]
    Tables(TablesCommand),

    /// Inspect configuration
    #[command(subcommand)]
    Config(ConfigCommand),

    /// Verify configuration, authentication, and Table API access
    Doctor,

    /// Inspect the command tree or an instance table schema
    Schema {
        /// Table whose dictionary metadata should be shown
        table: Option<String>,

        /// Return only one command and its arguments, such as "incidents list"
        #[arg(long, conflicts_with = "table")]
        command: Option<String>,

        /// Refresh metadata from the instance
        #[arg(long)]
        refresh: bool,
    },

    /// List the configured choices for a table field
    Choices {
        table: String,
        field: String,

        /// Refresh metadata from the instance
        #[arg(long)]
        refresh: bool,
    },

    /// Resolve a human user/group identifier to a ServiceNow record
    Resolve {
        #[arg(value_enum)]
        kind: ReferenceKind,
        value: String,
    },

    /// Generate shell completions
    Completions {
        /// Shell whose completion script to generate
        shell: Shell,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Sign in and store the credential securely when possible
    Login {
        /// Profile name to create or replace
        #[arg(default_value = "default")]
        profile: String,

        /// Instance name, hostname, or URL
        #[arg(long)]
        instance: Option<String>,

        /// Authentication method (auto-detected when omitted)
        #[arg(long, value_enum)]
        method: Option<AuthType>,

        /// Username for Basic authentication
        #[arg(long)]
        username: Option<String>,

        /// OAuth application client ID
        #[arg(long)]
        client_id: Option<String>,

        /// Space-separated OAuth scopes
        #[arg(long)]
        scope: Option<String>,

        /// Registered loopback OAuth redirect URI
        #[arg(long)]
        redirect_uri: Option<String>,

        /// Read the password/token/client secret from stdin
        #[arg(long)]
        secret_stdin: bool,

        /// Store the credential in the protected config file instead of the OS keychain
        #[arg(long)]
        insecure_storage: bool,

        /// Do not open a browser automatically (OAuth only)
        #[arg(long)]
        no_browser: bool,

        /// Block all writes when this profile is active
        #[arg(long)]
        read_only: bool,
    },

    /// Remove the stored credential but keep profile settings
    Logout {
        /// Profile whose credential should be removed
        profile: Option<String>,
    },

    /// Show the active identity and credential health
    Status,
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// List configured profiles
    List,

    /// Select the default profile for future commands
    Use { name: String },

    /// Remove a profile and its stored credential
    Remove {
        name: String,

        /// Confirm removal without prompting
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum IncidentsCommand {
    /// List incidents
    List {
        /// ServiceNow encoded query
        #[arg(short, long)]
        query: Option<String>,

        /// Return only active incidents
        #[arg(long)]
        active: bool,

        /// Maximum records to return
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,

        /// Skip the first N records
        #[arg(long, default_value = "0")]
        offset: usize,

        /// Fetch all pages
        #[arg(long)]
        all: bool,

        /// Comma-separated fields to return
        #[arg(long)]
        fields: Option<String>,

        /// Return raw values, display values, or both
        #[arg(long, value_enum)]
        display_value: Option<DisplayValue>,
    },

    /// List incidents assigned to the authenticated user
    Mine {
        /// Additional ServiceNow encoded query
        #[arg(short, long)]
        query: Option<String>,

        /// Maximum records to return
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,

        /// Fetch all pages
        #[arg(long)]
        all: bool,

        /// Return raw values, display values, or both
        #[arg(long, value_enum)]
        display_value: Option<DisplayValue>,
    },

    /// Show an incident by number or sys_id
    Show {
        /// Incident number (for example INC0010001) or a 32-character sys_id
        identifier: String,

        /// Comma-separated fields to return
        #[arg(long)]
        fields: Option<String>,

        /// Return raw values, display values, or both
        #[arg(long, value_enum, default_value = "all")]
        display_value: DisplayValue,
    },

    /// Create an incident
    Create {
        /// Short description
        #[arg(short, long)]
        short_description: String,

        /// Full description
        #[arg(short, long)]
        description: Option<String>,

        /// Category value
        #[arg(long)]
        category: Option<String>,

        /// Impact value
        #[arg(long)]
        impact: Option<String>,

        /// Urgency value
        #[arg(long)]
        urgency: Option<String>,

        /// Assignment group sys_id or accepted display value
        #[arg(long)]
        assignment_group: Option<String>,

        /// Assignee sys_id or accepted display value
        #[arg(long)]
        assignee: Option<String>,

        /// Additional field as name=value; repeatable
        #[arg(long = "field")]
        fields: Vec<String>,
    },

    /// Update an incident
    Update {
        /// Incident number or sys_id
        identifier: String,

        /// New short description
        #[arg(long)]
        short_description: Option<String>,

        /// New description
        #[arg(long)]
        description: Option<String>,

        /// State value
        #[arg(long)]
        state: Option<String>,

        /// Assignee sys_id or accepted display value
        #[arg(long)]
        assignee: Option<String>,

        /// Work note to append
        #[arg(long)]
        work_notes: Option<String>,

        /// Additional field as name=value; repeatable
        #[arg(long = "field")]
        fields: Vec<String>,
    },

    /// Edit an incident safely in your preferred editor
    Edit {
        /// Incident number or sys_id
        identifier: String,

        /// Read edited YAML from a file instead of opening an editor
        #[arg(long)]
        file: Option<std::path::PathBuf>,

        /// Preview the semantic patch without updating ServiceNow
        #[arg(long)]
        dry_run: bool,

        /// Apply without an interactive confirmation
        #[arg(long)]
        yes: bool,
    },

    /// Append a work note to an incident
    Note {
        /// Incident number or sys_id
        identifier: String,

        /// Work note text
        text: Option<String>,

        /// Read the work note from a file, or - for stdin
        #[arg(long)]
        file: Option<std::path::PathBuf>,

        /// Preview the update without sending it
        #[arg(long)]
        dry_run: bool,
    },

    /// Assign an incident using a user name, email, group name, or sys_id
    Assign {
        /// Incident number or sys_id
        identifier: String,

        /// User name, email, display name, @me, or sys_id
        #[arg(long = "to")]
        assignee: Option<String>,

        /// Assignment group name or sys_id
        #[arg(long)]
        group: Option<String>,

        /// Preview the update without sending it
        #[arg(long)]
        dry_run: bool,
    },

    /// Open an incident in the ServiceNow web interface
    Open {
        /// Incident number or sys_id
        identifier: String,

        /// Print the URL instead of opening a browser
        #[arg(long)]
        print: bool,
    },

    /// Watch an incident and stream field changes
    Watch {
        /// Incident number or sys_id
        identifier: String,

        /// Polling interval in seconds
        #[arg(long, default_value = "5")]
        interval: u64,

        /// Stop after this many polls; omit to watch until Ctrl-C
        #[arg(long)]
        count: Option<usize>,

        /// Comma-separated fields to watch
        #[arg(long)]
        fields: Option<String>,
    },
}

#[derive(Subcommand)]
enum AttachmentsCommand {
    /// List attachments on a record
    List {
        /// Table containing the record, such as incident or change_request
        table: String,

        /// Record number, sys_id, or same-instance form URL
        record: String,

        /// Maximum attachments to return
        #[arg(short = 'n', long, default_value = "100")]
        limit: usize,

        /// Fetch every attachment
        #[arg(long)]
        all: bool,
    },

    /// Upload a file to a record
    Upload {
        /// Table containing the record, such as incident or change_request
        table: String,

        /// Record number, sys_id, or same-instance form URL
        record: String,

        /// Local file to upload
        file: std::path::PathBuf,

        /// Attachment name; defaults to the local file name
        #[arg(long)]
        name: Option<String>,

        /// MIME type; inferred from the file name when omitted
        #[arg(long)]
        content_type: Option<String>,

        /// Preview the upload without sending file contents
        #[arg(long)]
        dry_run: bool,
    },

    /// Download an attachment by sys_id or same-instance Attachment API URL
    Download {
        /// Attachment sys_id or Attachment API URL
        attachment: String,

        /// File path, directory, or - for stdout; defaults to the attachment name
        destination: Option<std::path::PathBuf>,

        /// Replace an existing local file
        #[arg(long)]
        force: bool,
    },

    /// Permanently delete an attachment
    Delete {
        /// Attachment sys_id or same-instance Attachment API URL
        attachment: String,

        /// Confirm deletion without prompting
        #[arg(long)]
        yes: bool,

        /// Preview the deletion without changing ServiceNow
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum TablesCommand {
    /// List records from a table
    List {
        /// Table name, such as incident or cmdb_ci
        table: String,

        /// ServiceNow encoded query
        #[arg(short, long)]
        query: Option<String>,

        /// Comma-separated fields to return
        #[arg(long)]
        fields: Option<String>,

        /// Maximum records to return
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,

        /// Skip the first N records
        #[arg(long, default_value = "0")]
        offset: usize,

        /// Fetch all pages
        #[arg(long)]
        all: bool,

        /// Return raw values, display values, or both
        #[arg(long, value_enum, default_value = "false")]
        display_value: DisplayValue,
    },

    /// Get one record by sys_id
    Get {
        table: String,
        sys_id: String,

        /// Comma-separated fields to return
        #[arg(long)]
        fields: Option<String>,

        /// Return raw values, display values, or both
        #[arg(long, value_enum, default_value = "all")]
        display_value: DisplayValue,
    },

    /// Create a record
    Create {
        table: String,

        /// JSON object, or - to read one from stdin
        #[arg(long)]
        data: Option<String>,

        /// Field as name=value; repeatable and overrides --data
        #[arg(long = "field")]
        fields: Vec<String>,
    },

    /// Update a record by sys_id
    Update {
        table: String,
        sys_id: String,

        /// JSON object, or - to read one from stdin
        #[arg(long)]
        data: Option<String>,

        /// Field as name=value; repeatable and overrides --data
        #[arg(long = "field")]
        fields: Vec<String>,
    },

    /// Delete a record by sys_id
    Delete {
        table: String,
        sys_id: String,

        /// Confirm permanent deletion
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Show resolved configuration with the secret masked
    Show,

    /// Print the config path and an example configuration
    Init,

    /// Print the config file path
    Path,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let machine_errors = matches!(
        cli.output.as_str(),
        "json" | "jsonl" | "ndjson" | "yaml" | "yml" | "csv"
    ) || (cli.output == "auto"
        && (cli.json || !std::io::IsTerminal::is_terminal(&std::io::stdout())));
    if let Err(error) = run(cli).await {
        print_error(&error, machine_errors);
        std::process::exit(exit_code(&error));
    }
}

async fn run(cli: Cli) -> Result<(), ApiError> {
    let output = OutputConfig::new(&cli.output, cli.json, cli.quiet, cli.no_color)?;
    let verbose = cli.verbose;

    match cli.command {
        Command::Init {
            profile,
            instance,
            method,
            username,
            client_id,
            scope,
            redirect_uri,
            secret_stdin,
            insecure_storage,
            no_browser,
            read_only,
        } => {
            let authenticated = run_auth_login(
                &output,
                profile,
                instance,
                method,
                username,
                client_id,
                scope,
                redirect_uri,
                secret_stdin,
                insecure_storage,
                no_browser,
                read_only,
                verbose,
            )
            .await?;
            if authenticated && !output.json {
                println!("\nNext\n  servicenow doctor\n  servicenow incidents mine");
            }
            return Ok(());
        }
        Command::Auth(AuthCommand::Login {
            profile,
            instance,
            method,
            username,
            client_id,
            scope,
            redirect_uri,
            secret_stdin,
            insecure_storage,
            no_browser,
            read_only,
        }) => {
            run_auth_login(
                &output,
                profile,
                instance,
                method,
                username,
                client_id,
                scope,
                redirect_uri,
                secret_stdin,
                insecure_storage,
                no_browser,
                read_only,
                verbose,
            )
            .await?;
            return Ok(());
        }
        Command::Auth(AuthCommand::Logout { profile }) => {
            let profile = profile.unwrap_or(active_profile_name()?);
            let removed = delete_stored_credential(&profile)?;
            let value = serde_json::json!({
                "profile": profile,
                "loggedOut": removed,
            });
            if output.json {
                output.value(&value);
            } else if removed {
                println!("Logged out of profile {profile}.");
            } else {
                println!("Profile {profile} had no stored credential.");
            }
            return Ok(());
        }
        Command::Profile(ProfileCommand::List) => {
            let profiles = profile_summaries()?;
            if output.json {
                output.value(&serde_json::json!({"result": profiles}));
            } else {
                let records = profiles
                    .into_iter()
                    .map(serde_json::to_value)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| ApiError::Other(error.to_string()))?;
                print_records_or(
                    &records,
                    None,
                    output.color,
                    "No profiles yet. Start here: `servicenow init`.",
                );
            }
            return Ok(());
        }
        Command::Profile(ProfileCommand::Use { name }) => {
            use_profile(&name)?;
            if output.json {
                output.value(&serde_json::json!({"activeProfile": name}));
            } else {
                println!("Now using profile {name}.");
            }
            return Ok(());
        }
        Command::Profile(ProfileCommand::Remove { name, yes }) => {
            let confirmed = yes
                || (std::io::stdin().is_terminal()
                    && Confirm::new()
                        .with_prompt(format!(
                            "Remove profile '{name}' and its stored credential?"
                        ))
                        .default(false)
                        .interact()
                        .map_err(|error| ApiError::Other(error.to_string()))?);
            if !confirmed {
                return Err(ApiError::InvalidInput(
                    "profile removal cancelled; use --yes for non-interactive removal".into(),
                ));
            }
            let credential_removed = delete_stored_credential(&name)?;
            let removed = remove_profile(&name)?;
            if !removed && !credential_removed {
                return Err(ApiError::NotFound(format!("config profile '{name}'")));
            }
            if output.json {
                output.value(&serde_json::json!({"removed": true, "profile": name}));
            } else {
                println!("Removed profile {name}.");
            }
            return Ok(());
        }
        Command::Schema {
            table: None,
            command,
            ..
        } => {
            let root = Cli::command();
            let schema = if let Some(path) = command {
                let selected = find_command(&root, &path)?;
                command_document(selected, Some(&path))
            } else {
                command_document(&root, None)
            };
            output.value(&schema);
            return Ok(());
        }
        Command::Completions { shell } => {
            let mut command = Cli::command();
            clap_complete::generate(shell, &mut command, "servicenow", &mut std::io::stdout());
            return Ok(());
        }
        Command::Config(ConfigCommand::Init) => {
            if output.json {
                output.value(&init_document());
            } else {
                let document = init_document();
                println!(
                    "Config path: {}",
                    document["configPath"].as_str().unwrap_or("")
                );
                println!(
                    "\n{}",
                    toml::to_string_pretty(&document["example"])
                        .unwrap_or_else(|_| "[default]\ninstance = \"dev12345\"".into())
                );
                println!("Next: {}", document["initCommand"].as_str().unwrap_or(""));
            }
            return Ok(());
        }
        Command::Config(ConfigCommand::Path) => {
            if output.json {
                output.value(&serde_json::json!({ "configPath": config_path() }));
            } else {
                println!("{}", config_path().display());
            }
            return Ok(());
        }
        _ => {}
    }

    let mut config = Config::load(cli.instance, cli.username, cli.profile)?;
    if servicenow_cli::auth::refresh_if_needed(&mut config).await? {
        output.message("OAuth access token refreshed.");
    }
    if matches!(cli.command, Command::Config(ConfigCommand::Show)) {
        let masked = if matches!(config.auth_type, AuthType::Browser) {
            "****".into()
        } else {
            mask_secret(&config.secret)
        };
        let value = serde_json::json!({
            "configPath": config_path(),
            "profile": config.profile,
            "instance": config.instance,
            "username": config.username,
            "authType": config.auth_type.as_str(),
            "secretMasked": masked,
            "readOnly": config.read_only,
        });
        if output.json {
            output.value(&value);
        } else {
            print_record(&value, output.color);
        }
        return Ok(());
    }

    let client = ServiceNowClient::new_with_user_token(
        &config.instance,
        config.username.as_deref(),
        &config.secret,
        config.auth_type,
        config.browser_user_token.as_deref(),
    )?;

    match cli.command {
        Command::Incidents(command) => run_incidents(command, &client, &config, &output).await?,
        Command::Attachments(command) => {
            run_attachments(command, &client, &config, &output).await?
        }
        Command::Tables(command) => run_tables(command, &client, &config, &output).await?,
        Command::Schema {
            table: Some(table),
            refresh,
            ..
        } => run_table_schema(&table, refresh, &client, &config, &output).await?,
        Command::Choices {
            table,
            field,
            refresh,
        } => run_choices(&table, &field, refresh, &client, &config, &output).await?,
        Command::Resolve { kind, value } => {
            let record = metadata::resolve_reference(&client, kind, &value).await?;
            emit_record(&output, record);
        }
        Command::Doctor | Command::Auth(AuthCommand::Status) => {
            run_doctor(&client, &config, &output).await?
        }
        Command::Init { .. }
        | Command::Auth(_)
        | Command::Profile(_)
        | Command::Config(_)
        | Command::Schema { .. }
        | Command::Completions { .. } => {
            unreachable!()
        }
    }
    Ok(())
}

async fn run_table_schema(
    table: &str,
    refresh: bool,
    client: &ServiceNowClient,
    config: &Config,
    output: &OutputConfig,
) -> Result<(), ApiError> {
    let metadata = if refresh {
        metadata::sync_table(client, &config.profile, table).await?
    } else if let Some(metadata) = metadata::load(&config.profile, table)? {
        metadata
    } else {
        output.message("No cached metadata found; fetching it from the instance.");
        metadata::sync_table(client, &config.profile, table).await?
    };
    if output.json {
        output.value(&serde_json::json!({"result": metadata}));
    } else {
        println!(
            "{}  {} fields  {} choice fields\n",
            output.heading(&metadata.table),
            metadata.fields.len(),
            metadata.choices.len()
        );
        let records = metadata::metadata_as_records(&metadata);
        let fields = [
            "field".into(),
            "label".into(),
            "type".into(),
            "reference".into(),
            "mandatory".into(),
            "read_only".into(),
            "choices".into(),
        ];
        print_records(&records, Some(&fields), output.color);
    }
    Ok(())
}

async fn run_choices(
    table: &str,
    field: &str,
    refresh: bool,
    client: &ServiceNowClient,
    config: &Config,
    output: &OutputConfig,
) -> Result<(), ApiError> {
    let metadata = if refresh {
        metadata::sync_table(client, &config.profile, table).await?
    } else if let Some(metadata) = metadata::load(&config.profile, table)? {
        metadata
    } else {
        metadata::sync_table(client, &config.profile, table).await?
    };
    if metadata.field(field).is_none() {
        return Err(ApiError::NotFound(format!("field {table}.{field}")));
    }
    let choices = metadata.choices.get(field).cloned().unwrap_or_default();
    if output.json {
        output.value(&serde_json::json!({
            "table": table,
            "field": field,
            "result": choices,
        }));
    } else if choices.is_empty() {
        println!("No configured choices for {table}.{field}.");
    } else {
        let records = choices
            .into_iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ApiError::Other(error.to_string()))?;
        print_records(
            &records,
            Some(&["value".into(), "label".into(), "sequence".into()]),
            output.color,
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_auth_login(
    output: &OutputConfig,
    profile: String,
    instance: Option<String>,
    method: Option<AuthType>,
    username: Option<String>,
    client_id: Option<String>,
    scope: Option<String>,
    redirect_uri: Option<String>,
    secret_stdin: bool,
    insecure_storage: bool,
    no_browser: bool,
    read_only: bool,
    verbose: bool,
) -> Result<bool, ApiError> {
    servicenow_cli::config::validate_profile_name(&profile)?;
    if output.format == OutputFormat::Text && std::io::stdin().is_terminal() {
        output.message(&format!(
            "{}\n  Secure sign-in, tailored to your instance.",
            output.heading("Connect to ServiceNow")
        ));
    }
    let saved = if instance.is_none() {
        profile_defaults(&profile)?
    } else {
        None
    };
    let resumed = saved.is_some();
    let instance = required_prompt(
        instance.or_else(|| saved.as_ref().map(|value| value.instance.clone())),
        "ServiceNow instance",
    )?;
    let method = method.or(saved
        .as_ref()
        .and_then(|value| value.auth_type.as_deref())
        .map(str::parse)
        .transpose()?);
    let username = username.or_else(|| saved.as_ref().and_then(|value| value.username.clone()));
    let client_id = client_id.or_else(|| saved.as_ref().and_then(|value| value.client_id.clone()));
    let scope = scope
        .or_else(|| saved.as_ref().and_then(|value| value.oauth_scope.clone()))
        .unwrap_or_else(|| "useraccount".into());
    let redirect_uri = redirect_uri
        .or_else(|| saved.as_ref().and_then(|value| value.redirect_uri.clone()))
        .unwrap_or_else(|| "http://127.0.0.1:8484/callback".into());
    let read_only = read_only
        || saved
            .as_ref()
            .and_then(|value| value.read_only)
            .unwrap_or(false);
    if resumed {
        output.success(&format!("Resuming profile '{profile}'"));
    }
    let method = resolve_auth_method(output, &instance, method, client_id.as_deref()).await?;
    let mut username = username;
    if matches!(method, AuthType::Basic) {
        username = Some(required_prompt(username, "Username")?);
    }
    let configured_client_id = if matches!(method, AuthType::OAuth) {
        if client_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            print_oauth_admin_request(output, &redirect_uri);
            if std::io::stdin().is_terminal() {
                let ready = Confirm::new()
                    .with_prompt("Do you have the client ID ready?")
                    .default(true)
                    .interact()
                    .map_err(|error| {
                        ApiError::Other(format!("failed to confirm OAuth readiness: {error}"))
                    })?;
                if !ready {
                    save_oauth_draft(&profile, &instance, &scope, &redirect_uri, read_only)?;
                    let resume_command = format!("servicenow auth login {profile}");
                    if output.json {
                        output.value(&serde_json::json!({
                            "profile": profile,
                            "instance": instance,
                            "authType": "oauth",
                            "authenticated": false,
                            "status": "awaiting-client-id",
                            "resumeCommand": resume_command,
                        }));
                    } else {
                        output.success("Setup saved—no credential was stored");
                        println!(
                            "\nWhen your administrator sends the client ID, continue with:\n  {resume_command} --client-id YOUR_CLIENT_ID"
                        );
                    }
                    return Ok(false);
                }
            }
        }
        Some(required_prompt(client_id, "OAuth client ID")?)
    } else {
        None
    };
    let file_storage = if matches!(method, AuthType::Browser) {
        None
    } else {
        Some(choose_credential_storage(insecure_storage)?)
    };
    let credential = match method {
        AuthType::Basic => StoredCredential::Basic {
            password: read_login_secret(secret_stdin, "Password", false)?,
        },
        AuthType::Browser => {
            if secret_stdin {
                return Err(ApiError::InvalidInput(
                    "browser sign-in cannot be read from stdin; remove --secret-stdin".into(),
                ));
            }
            if !no_browser {
                output.message("\nOpening a private browser window for ServiceNow…");
                output.message(
                    "Complete your normal SSO sign-in there. This CLI never receives your identity-provider password.",
                );
            }
            let spinner = if verbose {
                output.message(
                    "Verbose browser diagnostics enabled; secrets, URLs, and page content are redacted.",
                );
                None
            } else {
                OnboardingSpinner::start(
                    output,
                    "Waiting for ServiceNow to complete the secure browser handoff",
                )
            };
            let credential = if verbose {
                let started = std::time::Instant::now();
                servicenow_cli::browser::browser_login_with_progress(
                    &instance,
                    !no_browser,
                    |progress| {
                        eprintln!(
                            "[+{:>6.1}s] browser: {}",
                            started.elapsed().as_secs_f64(),
                            progress.message()
                        );
                    },
                )
                .await
            } else {
                servicenow_cli::browser::browser_login(&instance, !no_browser).await
            };
            drop(spinner);
            credential?
        }
        AuthType::Bearer => StoredCredential::Bearer {
            access_token: read_login_secret(secret_stdin, "Access token", false)?,
        },
        AuthType::OAuth => {
            let id = configured_client_id
                .as_deref()
                .expect("OAuth client ID was collected before reading credentials");
            let client_secret = read_login_secret(
                secret_stdin,
                "OAuth client secret (optional; press Enter to skip)",
                true,
            )?;
            servicenow_cli::auth::oauth_login(
                &instance,
                id,
                (!client_secret.is_empty()).then_some(client_secret),
                Some(&scope),
                &redirect_uri,
                !no_browser,
            )
            .await?
        }
    };
    let browser_user_token = match &credential {
        StoredCredential::Browser { user_token, .. } => Some(user_token.as_str()),
        _ => None,
    };
    let client = ServiceNowClient::new_with_user_token(
        &instance,
        username.as_deref(),
        credential.secret(),
        method,
        browser_user_token,
    )?;
    let users = match client
        .list_records(
            "sys_user",
            &ListOptions {
                query: Some("sys_id=javascript:gs.getUserID()".into()),
                fields: Some(vec!["sys_id".into(), "user_name".into(), "name".into()]),
                limit: 1,
                ..ListOptions::default()
            },
        )
        .await
    {
        Ok(users) => users,
        Err(error) if matches!(method, AuthType::Basic) && matches!(error, ApiError::Auth(_)) => {
            return Err(enrich_basic_auth_error(&instance, error).await);
        }
        Err(error) => return Err(error),
    };
    let mut browser_identity = None;
    if let Some(user) = users.first() {
        username = field_text(user, "user_name").or(username);
        if matches!(method, AuthType::Browser) {
            let name = field_text(user, "name");
            browser_identity = match (name.as_deref(), username.as_deref()) {
                (Some(name), Some(username)) if name != username => {
                    Some(format!("{name} ({username})"))
                }
                (Some(name), _) => Some(name.into()),
                (_, Some(username)) => Some(username.into()),
                _ => None,
            };
        }
    }
    if matches!(method, AuthType::Browser) {
        let identity = browser_identity.unwrap_or_else(|| "your ServiceNow account".into());
        if output.format == OutputFormat::Text && std::io::stdin().is_terminal() {
            let accepted = Confirm::new()
                .with_prompt(format!("Continue as {identity}?"))
                .default(true)
                .interact()
                .map_err(|error| {
                    ApiError::Other(format!("failed to confirm browser identity: {error}"))
                })?;
            if !accepted {
                return Err(ApiError::InvalidInput(
                    "browser sign-in cancelled; no credential was stored. To use another account, set SERVICENOW_BROWSER to a browser without automatic work-account sign-in and retry"
                        .into(),
                ));
            }
        } else if output.format == OutputFormat::Text {
            output.success(&format!("Browser sign-in complete as {identity}"));
        }
    }

    let file_storage = match file_storage {
        Some(file_storage) => file_storage,
        None => choose_credential_storage(insecure_storage)?,
    };

    let mut profile_config = ProfileConfig::default();
    profile_config.instance = Some(instance);
    profile_config.username = username.clone();
    profile_config.auth_type = Some(method.as_str().into());
    profile_config.read_only = Some(read_only);
    profile_config.credential_store = Some(if file_storage { "file" } else { "keyring" }.into());
    profile_config.credential = file_storage.then_some(credential.clone());
    profile_config.client_id = configured_client_id;
    profile_config.oauth_scope = matches!(method, AuthType::OAuth).then_some(scope);
    profile_config.redirect_uri = matches!(method, AuthType::OAuth).then_some(redirect_uri);
    if !file_storage {
        credentials::store(&profile, &credential)?;
    }
    if let Err(error) = save_profile(&profile, profile_config, true) {
        if !file_storage {
            let _ = credentials::delete(&profile);
        }
        return Err(error);
    }

    let result = serde_json::json!({
        "profile": profile,
        "instance": client.site_url(),
        "username": username,
        "authType": method.as_str(),
        "readOnly": read_only,
        "credentialStore": if file_storage { "config-file" } else { "os-keychain" },
        "authenticated": true,
    });
    if output.json {
        output.value(&result);
    } else {
        output.success("Connected to ServiceNow");
        println!("\n{}", output.heading("Connection"));
        println!(
            "  Profile      {}",
            result["profile"].as_str().unwrap_or("")
        );
        println!(
            "  Instance     {}",
            result["instance"].as_str().unwrap_or("")
        );
        println!(
            "  User         {}",
            result["username"].as_str().unwrap_or("")
        );
        println!(
            "  Auth         {}",
            result["authType"].as_str().unwrap_or("")
        );
        println!(
            "  Safety       {}",
            if read_only {
                "read-only"
            } else {
                "writes enabled"
            }
        );
        println!(
            "  Credentials  {}",
            if file_storage {
                "config file (plaintext, mode 0600)"
            } else {
                "OS keychain"
            }
        );
    }
    Ok(true)
}

fn print_oauth_admin_request(output: &OutputConfig, redirect_uri: &str) {
    let heading = output.heading("ServiceNow OAuth app required");
    output.message(&format!(
        "\n{heading}\n\nAsk your ServiceNow administrator:\n\n  Please create an OAuth API endpoint for external clients under\n  System OAuth → Application Registry for the ServiceNow CLI, and\n  register this redirect URI:\n\n    {redirect_uri}\n\n  Then send me the client ID.\n\nMicrosoft Entra remains the browser sign-in; the OAuth app itself is\nconfigured in ServiceNow."
    ));
}

async fn enrich_basic_auth_error(instance: &str, error: ApiError) -> ApiError {
    let ApiError::Auth(message) = error else {
        return error;
    };
    match servicenow_cli::auth::discover_login_provider(instance).await {
        Ok(servicenow_cli::auth::LoginProvider::MicrosoftEntra) => ApiError::Auth(format!(
            "Basic authentication was rejected, and this instance appears to use Microsoft Entra SSO. Federated accounts usually do not have a usable ServiceNow password. Use browser sign-in instead; it needs no OAuth application or administrator setup. ServiceNow response: {message}"
        )),
        Ok(servicenow_cli::auth::LoginProvider::ExternalSso(host)) => {
            let provider = host
                .map(|host| format!(" through {host}"))
                .unwrap_or_default();
            ApiError::Auth(format!(
                "Basic authentication was rejected, and this instance appears to use external SSO{provider}. Federated accounts usually do not have a usable ServiceNow password. Use browser sign-in instead; it needs no OAuth application or administrator setup. ServiceNow response: {message}"
            ))
        }
        _ => ApiError::Auth(message),
    }
}

fn save_oauth_draft(
    profile: &str,
    instance: &str,
    scope: &str,
    redirect_uri: &str,
    read_only: bool,
) -> Result<(), ApiError> {
    let mut draft = ProfileConfig::default();
    draft.instance = Some(instance.into());
    draft.auth_type = Some("oauth".into());
    draft.read_only = Some(read_only);
    draft.oauth_scope = Some(scope.into());
    draft.redirect_uri = Some(redirect_uri.into());
    save_profile(profile, draft, true)
}

async fn resolve_auth_method(
    output: &OutputConfig,
    instance: &str,
    explicit: Option<AuthType>,
    client_id: Option<&str>,
) -> Result<AuthType, ApiError> {
    if let Some(method) = explicit {
        return Ok(method);
    }
    if client_id.is_some_and(|value| !value.trim().is_empty()) {
        return Ok(AuthType::OAuth);
    }
    let spinner = OnboardingSpinner::start(output, "Checking how this instance signs users in");
    if spinner.is_none() {
        output.message("Checking how this instance signs users in…");
    }
    let discovery = servicenow_cli::auth::discover_login_provider(instance).await;
    drop(spinner);
    match discovery {
        Ok(servicenow_cli::auth::LoginProvider::MicrosoftEntra) => {
            output.success("Microsoft Entra SSO detected");
            output.message(
                "Sign in through your browser—no ServiceNow password or OAuth application is required.",
            );
            Ok(AuthType::Browser)
        }
        Ok(servicenow_cli::auth::LoginProvider::ExternalSso(host)) => {
            let provider = host
                .as_deref()
                .map(|host| format!("External SSO detected ({host})"))
                .unwrap_or_else(|| "External SSO detected".into());
            output.success(&provider);
            output.message(
                "Sign in through your browser—no identity-provider password is entered into this CLI.",
            );
            Ok(AuthType::Browser)
        }
        Ok(servicenow_cli::auth::LoginProvider::Undetermined)
            if std::io::stdin().is_terminal() =>
        {
            output.message(
                "No SSO redirect was detected. A ServiceNow login page does not prove that your account has a local password.",
            );
            select_auth_method()
        }
        Ok(servicenow_cli::auth::LoginProvider::Undetermined) => Err(ApiError::InvalidInput(
            "the instance did not expose a definitive login method; pass --method browser, basic, oauth, or bearer"
                .into(),
        )),
        Err(error) if std::io::stdin().is_terminal() => {
            output.message(&format!(
                "Could not identify the login method automatically ({error})."
            ));
            select_auth_method()
        }
        Err(error) => Err(ApiError::InvalidInput(format!(
            "could not auto-detect the authentication method ({error}); pass --method browser, basic, oauth, or bearer"
        ))),
    }
}

struct OnboardingSpinner {
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl OnboardingSpinner {
    fn start(output: &OutputConfig, message: &str) -> Option<Self> {
        if output.quiet || output.format != OutputFormat::Text || !std::io::stderr().is_terminal() {
            return None;
        }
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let thread_running = running.clone();
        let message = message.to_string();
        let handle = std::thread::spawn(move || {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut frame = 0;
            while thread_running.load(std::sync::atomic::Ordering::Relaxed) {
                eprint!("\r{} {message}", frames[frame % frames.len()]);
                let _ = std::io::stderr().flush();
                frame += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
        });
        Some(Self {
            running,
            handle: Some(handle),
        })
    }
}

impl Drop for OnboardingSpinner {
    fn drop(&mut self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        eprint!("\r{:width$}\r", "", width = 72);
        let _ = std::io::stderr().flush();
    }
}

fn select_auth_method() -> Result<AuthType, ApiError> {
    let options = [
        "Browser sign-in (SSO, no administrator setup)",
        "Managed OAuth application",
        "ServiceNow username and password",
        "Access token",
    ];
    let selected = Select::new()
        .with_prompt("How do you sign in?")
        .items(options)
        .default(0)
        .interact()
        .map_err(|error| ApiError::Other(format!("failed to choose authentication: {error}")))?;
    Ok(match selected {
        0 => AuthType::Browser,
        1 => AuthType::OAuth,
        2 => AuthType::Basic,
        _ => AuthType::Bearer,
    })
}

fn choose_credential_storage(insecure_storage: bool) -> Result<bool, ApiError> {
    if insecure_storage {
        eprintln!(
            "warning: the credential will be stored in plaintext in {} (protected with mode 0600 on Unix)",
            config_path().display()
        );
        return Ok(true);
    }
    match credentials::available() {
        Ok(()) => Ok(false),
        Err(error) if std::io::stdin().is_terminal() => {
            eprintln!("warning: {error}");
            let confirmed = Confirm::new()
                .with_prompt(format!(
                    "Store the credential in plaintext in {} (mode 0600) instead?",
                    config_path().display()
                ))
                .default(false)
                .interact()
                .map_err(|prompt_error| {
                    ApiError::Other(format!(
                        "failed to choose credential storage: {prompt_error}"
                    ))
                })?;
            if confirmed {
                Ok(true)
            } else {
                Err(ApiError::InvalidInput(
                    "credential storage was cancelled; start a Secret Service provider or use environment variables"
                        .into(),
                ))
            }
        }
        Err(error) => Err(ApiError::InvalidInput(format!(
            "{error}; rerun with --insecure-storage to use the protected config file, or use environment variables"
        ))),
    }
}

fn required_prompt(value: Option<String>, prompt: &str) -> Result<String, ApiError> {
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        return Ok(value.trim().into());
    }
    if !std::io::stdin().is_terminal() {
        return Err(ApiError::InvalidInput(format!(
            "{prompt} is required in non-interactive mode"
        )));
    }
    Input::<String>::new()
        .with_prompt(prompt)
        .interact_text()
        .map_err(|error| ApiError::Other(format!("failed to read {prompt}: {error}")))
        .and_then(|value| {
            (!value.trim().is_empty())
                .then(|| value.trim().into())
                .ok_or_else(|| ApiError::InvalidInput(format!("{prompt} cannot be empty")))
        })
}

fn read_login_secret(
    from_stdin: bool,
    prompt: &str,
    allow_empty: bool,
) -> Result<String, ApiError> {
    let value = if from_stdin {
        let mut value = String::new();
        std::io::stdin()
            .read_to_string(&mut value)
            .map_err(|error| ApiError::Other(format!("failed to read secret: {error}")))?;
        value.trim_end_matches(['\r', '\n']).to_string()
    } else {
        if !std::io::stdin().is_terminal() {
            return Err(ApiError::InvalidInput(format!(
                "{prompt} requires --secret-stdin in non-interactive mode"
            )));
        }
        Password::new()
            .with_prompt(prompt)
            .allow_empty_password(allow_empty)
            .interact()
            .map_err(|error| ApiError::Other(format!("failed to read {prompt}: {error}")))?
    };
    if value.is_empty() && !allow_empty {
        Err(ApiError::InvalidInput(format!("{prompt} cannot be empty")))
    } else {
        Ok(value)
    }
}

async fn run_doctor(
    client: &ServiceNowClient,
    config: &Config,
    output: &OutputConfig,
) -> Result<(), ApiError> {
    let users = client
        .list_records(
            "sys_user",
            &ListOptions {
                query: Some("sys_id=javascript:gs.getUserID()".into()),
                fields: Some(vec![
                    "sys_id".into(),
                    "user_name".into(),
                    "name".into(),
                    "active".into(),
                ]),
                limit: 1,
                ..ListOptions::default()
            },
        )
        .await?;
    let user = users.first().ok_or_else(|| {
        ApiError::Other("authentication succeeded, but the current user was not returned".into())
    })?;
    let username = field_text(user, "user_name").unwrap_or_else(|| "unknown".into());

    client
        .list_records(
            "incident",
            &ListOptions {
                fields: Some(vec!["sys_id".into(), "number".into()]),
                limit: 1,
                ..ListOptions::default()
            },
        )
        .await?;

    let credential_detail = match config.credential_store() {
        "config-file" => "config file (plaintext, mode 0600)",
        "os-keychain" => "OS keychain",
        "environment" => "environment variable",
        "legacy-config" => "legacy plaintext config field",
        _ => "unknown",
    };
    let checks = serde_json::json!([
        {"name": "configuration", "ok": true, "detail": config.instance},
        {"name": "authentication", "ok": true, "detail": username},
        {"name": "credentials", "ok": true, "detail": credential_detail},
        {"name": "table_api", "ok": true, "detail": "incident table is readable"},
        {
            "name": "write_safety",
            "ok": true,
            "detail": if config.read_only { "read-only mode enabled" } else { "write operations enabled" }
        }
    ]);
    let result = serde_json::json!({
        "ok": true,
        "instance": client.site_url(),
        "checks": checks,
    });
    if output.json {
        output.value(&result);
    } else {
        println!("ServiceNow connection\n");
        for check in result["checks"].as_array().expect("checks are an array") {
            println!(
                "  ✓ {:<16} {}",
                check["name"].as_str().unwrap_or("check"),
                check["detail"].as_str().unwrap_or("")
            );
        }
        println!("\nReady.");
    }
    Ok(())
}

async fn run_incidents(
    command: IncidentsCommand,
    client: &ServiceNowClient,
    config: &Config,
    output: &OutputConfig,
) -> Result<(), ApiError> {
    match command {
        IncidentsCommand::List {
            query,
            active,
            limit,
            offset,
            all,
            fields,
            display_value,
        } => {
            let query = combine_query(active.then_some("active=true"), query.as_deref());
            let custom_fields = fields.is_some();
            let fields = parse_fields(fields.as_deref()).or_else(|| {
                Some(
                    INCIDENT_LIST_FIELDS
                        .iter()
                        .map(|field| (*field).into())
                        .collect(),
                )
            });
            let records = client
                .list_records(
                    "incident",
                    &ListOptions {
                        query,
                        fields: fields.clone(),
                        limit,
                        offset,
                        all,
                        display_value: display_value.unwrap_or_else(|| {
                            if output.format == OutputFormat::Text {
                                DisplayValue::True
                            } else {
                                DisplayValue::False
                            }
                        }),
                    },
                )
                .await?;
            let human_fields: Vec<String> = INCIDENT_HUMAN_FIELDS
                .iter()
                .map(|field| (*field).into())
                .collect();
            let visible_fields = if custom_fields {
                fields.as_deref()
            } else {
                Some(human_fields.as_slice())
            };
            emit_records_or(
                output,
                records,
                visible_fields,
                "No incidents matched. Try broadening --query or removing --active.",
            );
        }
        IncidentsCommand::Mine {
            query,
            limit,
            all,
            display_value,
        } => {
            let query = combine_query(
                Some("assigned_to=javascript:gs.getUserID()"),
                query.as_deref(),
            );
            let fields: Vec<String> = INCIDENT_LIST_FIELDS
                .iter()
                .map(|field| (*field).into())
                .collect();
            let records = client
                .list_records(
                    "incident",
                    &ListOptions {
                        query,
                        fields: Some(fields.clone()),
                        limit,
                        all,
                        display_value: display_value.unwrap_or_else(|| {
                            if output.format == OutputFormat::Text {
                                DisplayValue::True
                            } else {
                                DisplayValue::False
                            }
                        }),
                        ..ListOptions::default()
                    },
                )
                .await?;
            let human_fields: Vec<String> = INCIDENT_HUMAN_FIELDS
                .iter()
                .map(|field| (*field).into())
                .collect();
            emit_records_or(
                output,
                records,
                Some(&human_fields),
                "Nothing is assigned to you. You’re clear for now. Explore with `servicenow incidents list --active`.",
            );
        }
        IncidentsCommand::Show {
            identifier,
            fields,
            display_value,
        } => {
            let fields = parse_fields(fields.as_deref());
            let record =
                resolve_incident(client, &identifier, fields.clone(), display_value).await?;
            emit_record(output, record);
        }
        IncidentsCommand::Create {
            short_description,
            description,
            category,
            impact,
            urgency,
            assignment_group,
            assignee,
            fields,
        } => {
            config.require_writable()?;
            let mut body = build_body(None, &fields)?;
            insert(&mut body, "short_description", Some(short_description));
            insert(&mut body, "description", description);
            insert(&mut body, "category", category);
            insert(&mut body, "impact", impact);
            insert(&mut body, "urgency", urgency);
            insert(
                &mut body,
                "assignment_group",
                resolve_reference_id(client, ReferenceKind::Group, assignment_group).await?,
            );
            insert(
                &mut body,
                "assigned_to",
                resolve_reference_id(client, ReferenceKind::User, assignee).await?,
            );
            let record = client.create_record("incident", &body).await?;
            output.success("Incident created.");
            emit_record(output, record);
        }
        IncidentsCommand::Update {
            identifier,
            short_description,
            description,
            state,
            assignee,
            work_notes,
            fields,
        } => {
            config.require_writable()?;
            let mut body = build_body(None, &fields)?;
            insert(&mut body, "short_description", short_description);
            insert(&mut body, "description", description);
            insert(&mut body, "state", state);
            insert(
                &mut body,
                "assigned_to",
                resolve_reference_id(client, ReferenceKind::User, assignee).await?,
            );
            insert(&mut body, "work_notes", work_notes);
            if body.is_empty() {
                return Err(ApiError::InvalidInput(
                    "at least one field must be supplied".into(),
                ));
            }
            let existing = resolve_incident(
                client,
                &identifier,
                Some(vec!["sys_id".into()]),
                DisplayValue::False,
            )
            .await?;
            let sys_id = record_sys_id(&existing)?.to_string();
            let record = client.update_record("incident", &sys_id, &body).await?;
            output.success("Incident updated.");
            emit_record(output, record);
        }
        IncidentsCommand::Edit {
            identifier,
            file,
            dry_run,
            yes,
        } => {
            let existing = resolve_incident(client, &identifier, None, DisplayValue::All).await?;
            let sys_id = record_sys_id(&existing)?.to_string();
            let original = incident::edit_document(
                &existing,
                metadata::load(&config.profile, "incident")?.as_ref(),
            )?;
            let edited = match file {
                Some(path) => read_file_or_stdin(&path, "edited incident")?,
                None if std::io::stdin().is_terminal() => Editor::new()
                    .extension(".yaml")
                    .edit(&original)
                    .map_err(|error| ApiError::Other(format!("failed to open editor: {error}")))?
                    .ok_or_else(|| ApiError::InvalidInput("incident edit cancelled".into()))?,
                None => {
                    return Err(ApiError::InvalidInput(
                        "an interactive terminal or --file is required for incident edit".into(),
                    ));
                }
            };
            let body = incident::changed_fields(&existing, incident::parse_edit_document(&edited)?);
            if body.is_empty() {
                if output.json {
                    output.value(&serde_json::json!({
                        "changed": false,
                        "incident": identifier,
                        "changes": {},
                    }));
                } else {
                    println!("No changes to apply.");
                }
                return Ok(());
            }
            if dry_run {
                emit_mutation_plan(output, "update", &identifier, &body);
                return Ok(());
            }
            config.require_writable()?;
            if !yes {
                if !std::io::stdin().is_terminal() {
                    return Err(ApiError::InvalidInput(
                        "confirmation requires an interactive terminal; rerun with --yes".into(),
                    ));
                }
                let diff = incident::unified_diff(&original, &edited);
                if !diff.is_empty() {
                    eprintln!("{diff}");
                }
                let confirmed = Confirm::new()
                    .with_prompt(format!(
                        "Apply {} changed field(s) to {identifier}?",
                        body.len()
                    ))
                    .default(false)
                    .interact()
                    .map_err(|error| {
                        ApiError::Other(format!("failed to confirm update: {error}"))
                    })?;
                if !confirmed {
                    return Err(ApiError::InvalidInput("incident edit cancelled".into()));
                }
            }
            let record = client.update_record("incident", &sys_id, &body).await?;
            output.success(&format!("Updated {identifier} ({} field(s)).", body.len()));
            emit_record(output, record);
        }
        IncidentsCommand::Note {
            identifier,
            text,
            file,
            dry_run,
        } => {
            let note = match (text, file) {
                (Some(_), Some(_)) => {
                    return Err(ApiError::InvalidInput(
                        "provide work note text or --file, not both".into(),
                    ));
                }
                (Some(text), None) => text,
                (None, Some(path)) => read_file_or_stdin(&path, "work note")?,
                (None, None) => {
                    return Err(ApiError::InvalidInput(
                        "work note text or --file is required".into(),
                    ));
                }
            };
            if note.trim().is_empty() {
                return Err(ApiError::InvalidInput("work note cannot be empty".into()));
            }
            let existing = resolve_incident(
                client,
                &identifier,
                Some(vec!["sys_id".into()]),
                DisplayValue::False,
            )
            .await?;
            let mut body = Map::new();
            body.insert("work_notes".into(), Value::String(note));
            if dry_run {
                emit_mutation_plan(output, "append_work_note", &identifier, &body);
                return Ok(());
            }
            config.require_writable()?;
            let record = client
                .update_record("incident", record_sys_id(&existing)?, &body)
                .await?;
            output.success(&format!("Added a work note to {identifier}."));
            emit_record(output, record);
        }
        IncidentsCommand::Assign {
            identifier,
            assignee,
            group,
            dry_run,
        } => {
            if assignee.is_none() && group.is_none() {
                return Err(ApiError::InvalidInput(
                    "provide --to, --group, or both".into(),
                ));
            }
            let mut body = Map::new();
            insert(
                &mut body,
                "assigned_to",
                resolve_reference_id(client, ReferenceKind::User, assignee).await?,
            );
            insert(
                &mut body,
                "assignment_group",
                resolve_reference_id(client, ReferenceKind::Group, group).await?,
            );
            let existing = resolve_incident(
                client,
                &identifier,
                Some(vec!["sys_id".into()]),
                DisplayValue::False,
            )
            .await?;
            if dry_run {
                emit_mutation_plan(output, "assign", &identifier, &body);
                return Ok(());
            }
            config.require_writable()?;
            let record = client
                .update_record("incident", record_sys_id(&existing)?, &body)
                .await?;
            output.success(&format!("Assigned {identifier}."));
            emit_record(output, record);
        }
        IncidentsCommand::Open { identifier, print } => {
            let existing = resolve_incident(
                client,
                &identifier,
                Some(vec!["sys_id".into(), "number".into()]),
                DisplayValue::False,
            )
            .await?;
            let url = client.record_url("incident", record_sys_id(&existing)?);
            if output.json {
                output.value(&serde_json::json!({
                    "incident": field_text(&existing, "number").unwrap_or(identifier),
                    "url": url,
                }));
            } else if print || !std::io::stdout().is_terminal() {
                println!("{url}");
            } else {
                open::that(&url).map_err(|error| {
                    ApiError::Other(format!("failed to open ServiceNow in a browser: {error}"))
                })?;
                output.success("Opened incident in ServiceNow.");
            }
        }
        IncidentsCommand::Watch {
            identifier,
            interval,
            count,
            fields,
        } => {
            if interval == 0 {
                return Err(ApiError::InvalidInput(
                    "--interval must be greater than zero".into(),
                ));
            }
            if count == Some(0) {
                return Err(ApiError::InvalidInput(
                    "--count must be greater than zero".into(),
                ));
            }
            if output.format == OutputFormat::Csv {
                return Err(ApiError::InvalidInput(
                    "incident watch is a stream; use table, json, jsonl, or yaml output".into(),
                ));
            }
            let fields = parse_fields(fields.as_deref()).unwrap_or_else(|| {
                [
                    "sys_id",
                    "number",
                    "short_description",
                    "state",
                    "priority",
                    "assigned_to",
                    "assignment_group",
                    "sys_updated_on",
                ]
                .into_iter()
                .map(str::to_string)
                .collect()
            });
            let mut previous =
                resolve_incident(client, &identifier, Some(fields.clone()), DisplayValue::All)
                    .await?;
            let sys_id = record_sys_id(&previous)?.to_string();
            emit_watch_event(output, &identifier, 0, &[], Some(&previous));
            let mut polls = 0usize;
            loop {
                if count.is_some_and(|limit| polls >= limit) {
                    break;
                }
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        output.message("Watch stopped.");
                        break;
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_secs(interval)) => {}
                }
                polls += 1;
                let current = client
                    .get_record("incident", &sys_id, Some(&fields), DisplayValue::All)
                    .await?;
                let changes = incident::change_records(&previous, &current);
                if !changes.is_empty() {
                    emit_watch_event(output, &identifier, polls, &changes, None);
                }
                previous = current;
            }
        }
    }
    Ok(())
}

async fn resolve_reference_id(
    client: &ServiceNowClient,
    kind: ReferenceKind,
    value: Option<String>,
) -> Result<Option<String>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let record = metadata::resolve_reference(client, kind, &value).await?;
    Ok(Some(record_sys_id(&record)?.to_string()))
}

fn read_file_or_stdin(path: &std::path::Path, label: &str) -> Result<String, ApiError> {
    if path == std::path::Path::new("-") {
        let mut value = String::new();
        std::io::stdin()
            .read_to_string(&mut value)
            .map_err(|error| {
                ApiError::Other(format!("failed to read {label} from stdin: {error}"))
            })?;
        Ok(value)
    } else {
        std::fs::read_to_string(path).map_err(|error| {
            ApiError::Other(format!(
                "failed to read {label} from {}: {error}",
                path.display()
            ))
        })
    }
}

fn emit_mutation_plan(
    output: &OutputConfig,
    operation: &str,
    identifier: &str,
    body: &Map<String, Value>,
) {
    let plan = serde_json::json!({
        "dryRun": true,
        "operation": operation,
        "table": "incident",
        "incident": identifier,
        "changes": body,
    });
    if output.json {
        output.value(&plan);
    } else {
        println!("Dry run: {operation} {identifier}\n");
        print_record(&plan["changes"], output.color);
    }
}

fn emit_watch_event(
    output: &OutputConfig,
    identifier: &str,
    poll: usize,
    changes: &[Value],
    initial: Option<&Value>,
) {
    let event = serde_json::json!({
        "event": if initial.is_some() { "snapshot" } else { "change" },
        "incident": identifier,
        "poll": poll,
        "changes": changes,
        "record": initial,
    });
    match output.format {
        OutputFormat::Json | OutputFormat::JsonLines => println!(
            "{}",
            serde_json::to_string(&event).expect("watch event is serializable")
        ),
        OutputFormat::Yaml => print!(
            "---\n{}",
            serde_saphyr::to_string(&event).expect("watch event is serializable")
        ),
        OutputFormat::Text => {
            if let Some(record) = initial {
                println!("Watching {identifier}. Press Ctrl-C to stop.\n");
                print_record(record, output.color);
            } else {
                println!("\n{} changed", output.heading(identifier));
                print_records(
                    changes,
                    Some(&["field".into(), "before".into(), "after".into()]),
                    output.color,
                );
            }
        }
        OutputFormat::Csv => unreachable!("CSV watch output is rejected before polling"),
    }
}

async fn run_attachments(
    command: AttachmentsCommand,
    client: &ServiceNowClient,
    config: &Config,
    output: &OutputConfig,
) -> Result<(), ApiError> {
    match command {
        AttachmentsCommand::List {
            table,
            record: identifier,
            limit,
            all,
        } => {
            let record = record::resolve(
                client,
                &table,
                &identifier,
                Some(vec!["sys_id".into(), "number".into()]),
                DisplayValue::False,
            )
            .await?;
            let sys_id = record_sys_id(&record)?;
            let attachments = client.list_attachments(&table, sys_id, limit, all).await?;
            emit_attachments(output, attachments)?;
        }
        AttachmentsCommand::Upload {
            table,
            record: identifier,
            file,
            name,
            content_type,
            dry_run,
        } => {
            if !dry_run {
                config.require_writable()?;
            }
            let metadata = std::fs::metadata(&file).map_err(|error| {
                ApiError::InvalidInput(format!(
                    "cannot read attachment {}: {error}",
                    file.display()
                ))
            })?;
            if !metadata.is_file() {
                return Err(ApiError::InvalidInput(format!(
                    "attachment source is not a regular file: {}",
                    file.display()
                )));
            }
            std::fs::File::open(&file).map_err(|error| {
                ApiError::InvalidInput(format!(
                    "cannot open attachment {}: {error}",
                    file.display()
                ))
            })?;
            let file_name = attachment::upload_file_name(&file, name.as_deref())?;
            let content_type = attachment::content_type(&file, content_type.as_deref())?;
            let record = record::resolve(
                client,
                &table,
                &identifier,
                Some(vec!["sys_id".into(), "number".into()]),
                DisplayValue::False,
            )
            .await?;
            let table_sys_id = record_sys_id(&record)?;
            if dry_run {
                let plan = serde_json::json!({
                    "dryRun": true,
                    "operation": "upload_attachment",
                    "table": table,
                    "record": identifier,
                    "tableSysId": table_sys_id,
                    "file": file,
                    "fileName": file_name,
                    "contentType": content_type,
                    "sizeBytes": metadata.len(),
                });
                if output.json {
                    output.value(&plan);
                } else {
                    println!("Dry run: upload {file_name} to {table}/{identifier}\n");
                    print_record(&plan, output.color);
                }
                return Ok(());
            }
            let uploaded = client
                .upload_attachment_file(&table, table_sys_id, &file_name, &content_type, &file)
                .await?;
            output.success(&format!(
                "Uploaded {file_name} ({}) to {table}/{identifier}.",
                attachment::human_size(&uploaded.size_bytes)
            ));
            emit_attachment(output, uploaded)?;
        }
        AttachmentsCommand::Download {
            attachment: identifier,
            destination,
            force,
        } => {
            let sys_id = record::attachment_sys_id(client.site_url(), &identifier)?;
            let metadata = client.get_attachment(&sys_id).await?;
            let destination =
                attachment::destination_path(destination.as_deref(), &metadata.file_name)?;
            if destination == std::path::Path::new("-") {
                let stdout = std::io::stdout();
                let mut writer = stdout.lock();
                let bytes = client.download_attachment(&sys_id, &mut writer).await?;
                output.message(&format!(
                    "Downloaded {} ({}) to stdout.",
                    metadata.file_name,
                    attachment::human_size(&bytes.to_string())
                ));
                return Ok(());
            }
            if destination.exists() && !force {
                return Err(ApiError::Conflict(format!(
                    "{} already exists; use --force to replace it",
                    destination.display()
                )));
            }
            let parent = destination
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| std::path::Path::new("."));
            if !parent.is_dir() {
                return Err(ApiError::InvalidInput(format!(
                    "destination directory does not exist: {}",
                    parent.display()
                )));
            }
            let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
                ApiError::Other(format!(
                    "failed to create a temporary download in {}: {error}",
                    parent.display()
                ))
            })?;
            let bytes = client.download_attachment(&sys_id, &mut temporary).await?;
            temporary.as_file().sync_all().map_err(|error| {
                ApiError::Other(format!("failed to sync downloaded attachment: {error}"))
            })?;
            if force {
                temporary.persist(&destination).map_err(|error| {
                    ApiError::Other(format!(
                        "failed to save attachment to {}: {}",
                        destination.display(),
                        error.error
                    ))
                })?;
            } else {
                temporary.persist_noclobber(&destination).map_err(|error| {
                    ApiError::Conflict(format!(
                        "could not save {} without replacing a file: {}",
                        destination.display(),
                        error.error
                    ))
                })?;
            }
            let result = serde_json::json!({
                "downloaded": true,
                "attachment": sys_id,
                "fileName": metadata.file_name,
                "contentType": metadata.content_type,
                "path": destination,
                "sizeBytes": bytes,
            });
            output.success(&format!(
                "Downloaded {} ({}) to {}.",
                result["fileName"].as_str().unwrap_or("attachment"),
                attachment::human_size(&bytes.to_string()),
                destination.display()
            ));
            if output.json {
                output.value(&result);
            } else {
                println!("{}", destination.display());
            }
        }
        AttachmentsCommand::Delete {
            attachment: identifier,
            yes,
            dry_run,
        } => {
            if !dry_run {
                config.require_writable()?;
            }
            let sys_id = record::attachment_sys_id(client.site_url(), &identifier)?;
            let metadata = client.get_attachment(&sys_id).await?;
            if dry_run {
                let plan = serde_json::json!({
                    "dryRun": true,
                    "operation": "delete_attachment",
                    "attachment": sys_id,
                    "fileName": metadata.file_name,
                    "table": metadata.table_name,
                    "tableSysId": metadata.table_sys_id,
                    "sizeBytes": metadata.size_bytes,
                });
                if output.json {
                    output.value(&plan);
                } else {
                    println!("Dry run: permanently delete {}\n", metadata.file_name);
                    print_record(&plan, output.color);
                }
                return Ok(());
            }
            let confirmed = yes
                || (std::io::stdin().is_terminal()
                    && Confirm::new()
                        .with_prompt(format!(
                            "Permanently delete '{}' ({})?",
                            metadata.file_name,
                            attachment::human_size(&metadata.size_bytes)
                        ))
                        .default(false)
                        .interact()
                        .map_err(|error| {
                            ApiError::Other(format!("failed to confirm deletion: {error}"))
                        })?);
            if !confirmed {
                return Err(ApiError::InvalidInput(
                    "attachment deletion cancelled; use --yes for non-interactive deletion".into(),
                ));
            }
            client.delete_attachment(&sys_id).await?;
            let result = serde_json::json!({
                "deleted": true,
                "attachment": sys_id,
                "fileName": metadata.file_name,
                "table": metadata.table_name,
                "tableSysId": metadata.table_sys_id,
            });
            output.success(&format!(
                "Deleted attachment {}.",
                result["fileName"].as_str().unwrap_or("attachment")
            ));
            if output.json {
                output.value(&result);
            } else {
                println!(
                    "Deleted {}.",
                    result["fileName"].as_str().unwrap_or("attachment")
                );
            }
        }
    }
    Ok(())
}

fn emit_attachments(
    output: &OutputConfig,
    attachments: Vec<servicenow_cli::api::AttachmentMetadata>,
) -> Result<(), ApiError> {
    let mut records = attachments
        .into_iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            ApiError::Other(format!("failed to encode attachment metadata: {error}"))
        })?;
    if output.json {
        emit_records(output, records, None);
    } else {
        for record in &mut records {
            if let Some(object) = record.as_object_mut() {
                let size = object
                    .get("size_bytes")
                    .and_then(Value::as_str)
                    .map(attachment::human_size)
                    .unwrap_or_else(|| "-".into());
                object.insert("size".into(), Value::String(size));
            }
        }
        let fields = [
            "file_name".into(),
            "content_type".into(),
            "size".into(),
            "sys_created_by".into(),
            "sys_created_on".into(),
            "sys_id".into(),
        ];
        print_records(&records, Some(&fields), output.color);
    }
    Ok(())
}

fn emit_attachment(
    output: &OutputConfig,
    attachment: servicenow_cli::api::AttachmentMetadata,
) -> Result<(), ApiError> {
    let record = serde_json::to_value(attachment).map_err(|error| {
        ApiError::Other(format!("failed to encode attachment metadata: {error}"))
    })?;
    emit_record(output, record);
    Ok(())
}

async fn run_tables(
    command: TablesCommand,
    client: &ServiceNowClient,
    config: &Config,
    output: &OutputConfig,
) -> Result<(), ApiError> {
    match command {
        TablesCommand::List {
            table,
            query,
            fields,
            limit,
            offset,
            all,
            display_value,
        } => {
            let fields = parse_fields(fields.as_deref());
            let records = client
                .list_records(
                    &table,
                    &ListOptions {
                        query,
                        fields: fields.clone(),
                        limit,
                        offset,
                        all,
                        display_value,
                    },
                )
                .await?;
            emit_records(output, records, fields.as_deref());
        }
        TablesCommand::Get {
            table,
            sys_id,
            fields,
            display_value,
        } => {
            let fields = parse_fields(fields.as_deref());
            let record = client
                .get_record(&table, &sys_id, fields.as_deref(), display_value)
                .await?;
            emit_record(output, record);
        }
        TablesCommand::Create {
            table,
            data,
            fields,
        } => {
            config.require_writable()?;
            let body = build_body(data.as_deref(), &fields)?;
            let record = client.create_record(&table, &body).await?;
            output.message("Record created.");
            emit_record(output, record);
        }
        TablesCommand::Update {
            table,
            sys_id,
            data,
            fields,
        } => {
            config.require_writable()?;
            let body = build_body(data.as_deref(), &fields)?;
            let record = client.update_record(&table, &sys_id, &body).await?;
            output.message("Record updated.");
            emit_record(output, record);
        }
        TablesCommand::Delete { table, sys_id, yes } => {
            config.require_writable()?;
            if !yes {
                return Err(ApiError::InvalidInput(
                    "deletion is permanent; rerun with --yes to confirm".into(),
                ));
            }
            client.delete_record(&table, &sys_id).await?;
            let result = serde_json::json!({
                "deleted": true,
                "table": table,
                "sys_id": sys_id,
            });
            if output.json {
                output.value(&result);
            } else {
                println!("Deleted {table}/{sys_id}.");
            }
        }
    }
    Ok(())
}

async fn resolve_incident(
    client: &ServiceNowClient,
    identifier: &str,
    fields: Option<Vec<String>>,
    display_value: DisplayValue,
) -> Result<Value, ApiError> {
    record::resolve(client, "incident", identifier, fields, display_value).await
}

fn emit_records(output: &OutputConfig, records: Vec<Value>, fields: Option<&[String]>) {
    emit_records_or(output, records, fields, "No records found.");
}

fn emit_records_or(
    output: &OutputConfig,
    records: Vec<Value>,
    fields: Option<&[String]>,
    empty_message: &str,
) {
    if output.json {
        output.value(&serde_json::json!({
            "count": records.len(),
            "result": records,
        }));
    } else {
        print_records_or(&records, fields, output.color, empty_message);
    }
}

fn emit_record(output: &OutputConfig, record: Value) {
    if output.json {
        output.value(&serde_json::json!({ "result": record }));
    } else {
        print_record(&record, output.color);
    }
}

fn combine_query(first: Option<&str>, second: Option<&str>) -> Option<String> {
    match (
        first.filter(|value| !value.is_empty()),
        second.filter(|value| !value.is_empty()),
    ) {
        (Some(first), Some(second)) => Some(format!("{first}^{second}")),
        (Some(value), None) | (None, Some(value)) => Some(value.to_string()),
        (None, None) => None,
    }
}

fn insert(body: &mut Map<String, Value>, name: &str, value: Option<String>) {
    if let Some(value) = value {
        body.insert(name.into(), Value::String(value));
    }
}

fn field_text(record: &Value, field: &str) -> Option<String> {
    let value = record.get(field)?;
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Object(value) => value
            .get("display_value")
            .and_then(Value::as_str)
            .or_else(|| value.get("value").and_then(Value::as_str))
            .map(str::to_string),
        _ => None,
    }
}

fn mask_secret(secret: &str) -> String {
    if secret.chars().count() <= 4 {
        "****".into()
    } else {
        let suffix: String = secret
            .chars()
            .rev()
            .take(4)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("***{suffix}")
    }
}

fn command_document(command: &clap::Command, selected_path: Option<&str>) -> Value {
    let path = selected_path.unwrap_or(command.get_name());
    let mut schema = command_schema(command, path);
    let object = schema
        .as_object_mut()
        .expect("command schema is always an object");
    object.insert("schemaVersion".into(), Value::String("1.0".into()));
    object.insert(
        "outputContract".into(),
        serde_json::json!({
            "default": "human table on a terminal; JSON when piped",
            "listEnvelope": {"count": "integer", "result": "array"},
            "recordEnvelope": {"result": "object"},
            "errorEnvelope": {"error": {"kind": "string", "message": "string", "remediation": "string|null"}},
            "streams": {"stdout": "data", "stderr": "status and errors"},
            "exitCodes": {
                "0": "success", "1": "unexpected", "2": "invalid_input", "3": "auth",
                "4": "not_found", "5": "api_error", "6": "rate_limit", "7": "conflict"
            }
        }),
    );
    schema
}

fn find_command<'a>(root: &'a clap::Command, path: &str) -> Result<&'a clap::Command, ApiError> {
    let mut command = root;
    let mut parts = path.split_whitespace().peekable();
    if parts.peek().is_none() {
        return Err(ApiError::InvalidInput("--command cannot be empty".into()));
    }
    if parts.peek().copied() == Some(root.get_name()) {
        parts.next();
    }
    for part in parts {
        command = command.find_subcommand(part).ok_or_else(|| {
            ApiError::NotFound(format!(
                "command '{path}'; inspect available commands with `servicenow schema`"
            ))
        })?;
    }
    Ok(command)
}

fn command_schema(command: &clap::Command, path: &str) -> Value {
    let args: Vec<Value> = command
        .get_arguments()
        .filter(|arg| arg.get_id() != "help" && arg.get_id() != "version")
        .map(|arg| {
            let range = arg.get_num_args();
            let takes_value = !matches!(
                arg.get_action(),
                clap::ArgAction::SetTrue | clap::ArgAction::SetFalse | clap::ArgAction::Count
            );
            let minimum = range
                .map(|range| range.min_values())
                .unwrap_or(usize::from(takes_value && arg.is_required_set()));
            let max_values = range
                .and_then(|range| {
                    (range.max_values() != usize::MAX).then_some(range.max_values())
                })
                .or(takes_value.then_some(1));
            let possible_values: Vec<String> = arg
                .get_possible_values()
                .into_iter()
                .map(|value| value.get_name().to_string())
                .collect();
            let default_values: Vec<String> = arg
                .get_default_values()
                .iter()
                .map(|value| value.to_string_lossy().into_owned())
                .collect();
            serde_json::json!({
                "id": arg.get_id().as_str(),
                "long": arg.get_long(),
                "short": arg.get_short().map(|value| value.to_string()),
                "required": arg.is_required_set(),
                "help": arg.get_help().map(|value| value.to_string()),
                "type": argument_type(arg, &possible_values),
                "action": argument_action(arg.get_action()),
                "valueCardinality": {
                    "minimum": minimum,
                    "maximum": max_values,
                },
                "defaultValues": default_values,
                "possibleValues": possible_values,
                "repeatable": matches!(arg.get_action(), clap::ArgAction::Append | clap::ArgAction::Count),
                "global": arg.is_global_set(),
                "environment": arg.get_env().map(|value| value.to_string_lossy().into_owned()),
                "valueHint": format!("{:?}", arg.get_value_hint()).to_lowercase(),
                "dynamicDefault": argument_dynamic_default(path, arg.get_id().as_str()),
            })
        })
        .collect();
    let commands: Vec<Value> = command
        .get_subcommands()
        .map(|subcommand| {
            let subcommand_path = format!("{path} {}", subcommand.get_name());
            command_schema(subcommand, &subcommand_path)
        })
        .collect();
    serde_json::json!({
        "name": command.get_name(),
        "path": path,
        "about": command.get_about().map(|value| value.to_string()),
        "arguments": args,
        "commands": commands,
        "behavior": command_behavior(path),
    })
}

fn argument_dynamic_default(path: &str, argument: &str) -> Option<&'static str> {
    if argument == "display_value" {
        return Some("display values for text output; raw values for machine output");
    }
    if path.ends_with("init") || path.ends_with("auth login") {
        return match argument {
            "instance" => Some("saved profile value when resuming; otherwise prompted"),
            "method" => Some(
                "saved profile value when resuming; otherwise detected from the instance login route",
            ),
            "scope" => Some("saved profile value when resuming; otherwise useraccount"),
            "redirect_uri" => {
                Some("saved profile value when resuming; otherwise http://127.0.0.1:8484/callback")
            }
            _ => None,
        };
    }
    None
}

fn argument_type(arg: &clap::Arg, possible_values: &[String]) -> &'static str {
    match arg.get_action() {
        clap::ArgAction::SetTrue | clap::ArgAction::SetFalse => "boolean",
        clap::ArgAction::Count => "integer",
        _ if !possible_values.is_empty() => "enum",
        _ if matches!(
            arg.get_id().as_str(),
            "limit" | "offset" | "interval" | "count"
        ) =>
        {
            "integer"
        }
        _ => "string",
    }
}

fn argument_action(action: &clap::ArgAction) -> &'static str {
    match action {
        clap::ArgAction::Set => "set",
        clap::ArgAction::Append => "append",
        clap::ArgAction::SetTrue => "set_true",
        clap::ArgAction::SetFalse => "set_false",
        clap::ArgAction::Count => "count",
        clap::ArgAction::Help => "help",
        clap::ArgAction::HelpShort => "help_short",
        clap::ArgAction::HelpLong => "help_long",
        clap::ArgAction::Version => "version",
        _ => "other",
    }
}

fn command_behavior(path: &str) -> Value {
    let remote_mutations = [
        "incidents create",
        "incidents update",
        "incidents edit",
        "incidents note",
        "incidents assign",
        "attachments upload",
        "attachments delete",
        "tables create",
        "tables update",
        "tables delete",
    ];
    let local_mutations = [
        "init",
        "auth login",
        "auth logout",
        "profile use",
        "profile remove",
        "attachments download",
    ];
    let mutation = remote_mutations.iter().any(|suffix| path.ends_with(suffix))
        || local_mutations.iter().any(|suffix| path.ends_with(suffix));
    let remote_mutation = remote_mutations.iter().any(|suffix| path.ends_with(suffix));
    let local_mutation = local_mutations.iter().any(|suffix| path.ends_with(suffix));
    let side_effect = if path.ends_with("init") || path.ends_with("auth login") {
        "local_and_remote"
    } else if remote_mutation {
        "remote"
    } else if local_mutation {
        "local"
    } else {
        "none"
    };
    let network_access = if path == "servicenow"
        || path == "auth"
        || path == "schema"
        || path.ends_with(" auth")
        || path.ends_with(" schema")
    {
        "conditional"
    } else if path == "profile"
        || path == "config"
        || path.ends_with(" profile")
        || path.ends_with(" config")
        || path.ends_with("completions")
        || path.ends_with("profile list")
        || path.ends_with("profile use")
        || path.ends_with("profile remove")
        || path.ends_with("auth logout")
        || path.ends_with("config show")
        || path.ends_with("config init")
        || path.ends_with("config path")
    {
        "none"
    } else {
        "required"
    };
    let destructive = path.ends_with("attachments delete")
        || path.ends_with("tables delete")
        || path.ends_with("profile remove");
    let requires_confirmation = destructive || path.ends_with("incidents edit");
    let supports_dry_run = [
        "incidents edit",
        "incidents note",
        "incidents assign",
        "attachments upload",
        "attachments delete",
    ]
    .iter()
    .any(|suffix| path.ends_with(suffix));
    serde_json::json!({
        "sideEffect": side_effect,
        "networkAccess": network_access,
        "mutation": mutation,
        "destructive": destructive,
        "requiresConfirmation": requires_confirmation,
        "supportsDryRun": supports_dry_run,
    })
}
