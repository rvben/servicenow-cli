use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use serde_json::{Map, Value};
use servicenow_cli::api::{ApiError, DisplayValue, ListOptions, ServiceNowClient};
use servicenow_cli::commands::{
    INCIDENT_LIST_FIELDS, build_body, parse_fields, print_record, print_records, record_sys_id,
};
use servicenow_cli::config::{Config, config_path, init_document};
use servicenow_cli::output::{OutputConfig, exit_code, print_error};

#[derive(Parser)]
#[command(
    name = "servicenow",
    version,
    about = "Agent-friendly CLI for ServiceNow",
    arg_required_else_help = true
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

    /// Output format: auto, text, or json
    #[arg(short, long, global = true, default_value = "auto")]
    output: String,

    /// Output JSON (alias for --output json)
    #[arg(long, global = true, hide = true)]
    json: bool,

    /// Suppress non-data messages
    #[arg(long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Work with incidents
    #[command(subcommand, visible_alias = "incident")]
    Incidents(IncidentsCommand),

    /// Perform generic CRUD operations through the Table API
    #[command(subcommand, visible_alias = "table")]
    Tables(TablesCommand),

    /// Inspect configuration
    #[command(subcommand)]
    Config(ConfigCommand),

    /// Verify configuration, authentication, and Table API access
    Doctor,

    /// Print the command tree as JSON for agent introspection
    Schema,

    /// Generate shell completions
    Completions {
        /// Shell whose completion script to generate
        shell: Shell,
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
        #[arg(long, value_enum, default_value = "false")]
        display_value: DisplayValue,
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
    let machine_errors = cli.output == "json"
        || (cli.output == "auto"
            && (cli.json || !std::io::IsTerminal::is_terminal(&std::io::stdout())));
    if let Err(error) = run(cli).await {
        print_error(&error, machine_errors);
        std::process::exit(exit_code(&error));
    }
}

async fn run(cli: Cli) -> Result<(), ApiError> {
    let output = OutputConfig::new(&cli.output, cli.json, cli.quiet)?;

    match cli.command {
        Command::Schema => {
            output.value(&command_schema(&Cli::command()));
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
                println!(
                    "{}",
                    document["recommendedPermissions"].as_str().unwrap_or("")
                );
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

    let config = Config::load(cli.instance, cli.username, cli.profile)?;
    if matches!(cli.command, Command::Config(ConfigCommand::Show)) {
        let masked = mask_secret(&config.secret);
        let value = serde_json::json!({
            "configPath": config_path(),
            "profile": config.profile,
            "instance": config.instance,
            "username": config.username,
            "authType": match config.auth_type {
                servicenow_cli::config::AuthType::Basic => "basic",
                servicenow_cli::config::AuthType::Bearer => "bearer",
            },
            "secretMasked": masked,
            "readOnly": config.read_only,
        });
        if output.json {
            output.value(&value);
        } else {
            print_record(&value);
        }
        return Ok(());
    }

    let client = ServiceNowClient::new(
        &config.instance,
        config.username.as_deref(),
        &config.secret,
        config.auth_type,
    )?;

    match cli.command {
        Command::Incidents(command) => run_incidents(command, &client, &config, &output).await?,
        Command::Tables(command) => run_tables(command, &client, &config, &output).await?,
        Command::Doctor => run_doctor(&client, &config, &output).await?,
        Command::Config(_) | Command::Schema | Command::Completions { .. } => unreachable!(),
    }
    Ok(())
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

    let checks = serde_json::json!([
        {"name": "configuration", "ok": true, "detail": config.instance},
        {"name": "authentication", "ok": true, "detail": username},
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
                        display_value,
                    },
                )
                .await?;
            emit_records(output, records, fields.as_deref());
        }
        IncidentsCommand::Mine { query, limit, all } => {
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
                        ..ListOptions::default()
                    },
                )
                .await?;
            emit_records(output, records, Some(&fields));
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
            insert(&mut body, "assignment_group", assignment_group);
            insert(&mut body, "assigned_to", assignee);
            let record = client.create_record("incident", &body).await?;
            output.message("Incident created.");
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
            insert(&mut body, "assigned_to", assignee);
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
            output.message("Incident updated.");
            emit_record(output, record);
        }
    }
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
    if identifier.len() == 32 && identifier.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        client
            .get_record("incident", identifier, fields.as_deref(), display_value)
            .await
    } else {
        client
            .find_one("incident", "number", identifier, fields, display_value)
            .await
    }
}

fn emit_records(output: &OutputConfig, records: Vec<Value>, fields: Option<&[String]>) {
    if output.json {
        output.value(&serde_json::json!({
            "count": records.len(),
            "result": records,
        }));
    } else {
        print_records(&records, fields);
    }
}

fn emit_record(output: &OutputConfig, record: Value) {
    if output.json {
        output.value(&serde_json::json!({ "result": record }));
    } else {
        print_record(&record);
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

fn command_schema(command: &clap::Command) -> Value {
    let args: Vec<Value> = command
        .get_arguments()
        .filter(|arg| arg.get_id() != "help" && arg.get_id() != "version")
        .map(|arg| {
            serde_json::json!({
                "id": arg.get_id().as_str(),
                "long": arg.get_long(),
                "short": arg.get_short().map(|value| value.to_string()),
                "required": arg.is_required_set(),
                "help": arg.get_help().map(|value| value.to_string()),
            })
        })
        .collect();
    let commands: Vec<Value> = command.get_subcommands().map(command_schema).collect();
    serde_json::json!({
        "name": command.get_name(),
        "about": command.get_about().map(|value| value.to_string()),
        "arguments": args,
        "commands": commands,
    })
}
