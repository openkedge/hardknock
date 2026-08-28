// SPDX-License-Identifier: Apache-2.0
use super::{Cli, Commands};
use crate::{
    Error, Result,
    bridge::{
        config::Config,
        protocol::*,
        transport::{self, BridgeClient},
    },
    cancellation::Cancellation,
    integrations,
};
use clap::{Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

#[derive(Debug, Subcommand)]
pub enum BridgeCommand {
    Start {
        #[arg(long)]
        foreground: bool,
        #[arg(long)]
        tcp: Option<u16>,
    },
    Status,
    Stop,
    Sessions,
    Inspect {
        id: String,
    },
    /// Send one versioned lifecycle payload from stdin (token is supplied locally).
    Call,
}
#[derive(Debug, Subcommand)]
pub enum IntegrationCommand {
    List,
    Doctor,
    Claude {
        #[command(subcommand)]
        command: AdapterCommand,
    },
    Codex {
        #[command(subcommand)]
        command: CodexCommand,
    },
    Hermes {
        #[command(subcommand)]
        command: AdapterCommand,
    },
    Openclaw {
        #[command(subcommand)]
        command: AdapterCommand,
    },
}
#[derive(Debug, Subcommand)]
pub enum AdapterCommand {
    Install {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    Uninstall {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    Check,
}
#[derive(Debug, Subcommand)]
pub enum CodexCommand {
    Check {
        #[arg(long, default_value = "codex")]
        executable: String,
        #[arg(long)]
        allow_untested: bool,
    },
    Run {
        #[arg(long, default_value = "codex")]
        executable: String,
        #[arg(long)]
        allow_untested: bool,
        #[arg(long)]
        resume: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 300)]
        timeout_secs: u64,
        task: String,
    },
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum HookAgent {
    Claude,
}
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    Capabilities,
}
#[derive(Debug, Subcommand)]
pub enum EventsCommand {
    Tail {
        #[arg(long)]
        follow: bool,
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
}
fn invalid(s: &str) -> Error {
    Error::InvalidInput(s.into())
}
fn input() -> Result<Value> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_EVENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(invalid("Event exceeds 1 MiB"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}
pub async fn ensure_started(home: &Path) -> Result<()> {
    let mut client = BridgeClient::new(home);
    client.timeout = Duration::from_millis(500);
    if client.request(AgentEvent::Status).await.is_ok() {
        return Ok(());
    }
    if !Config::load(home)?.bridge.autostart {
        return Err(invalid("Bridge unavailable; autostart disabled"));
    }
    start(home, None).await
}
pub async fn start(home: &Path, tcp: Option<u16>) -> Result<()> {
    let mut client = BridgeClient::new(home);
    client.timeout = Duration::from_secs(1);
    if client.request(AgentEvent::Status).await.is_ok() {
        return Ok(());
    }
    // Explicitly detach, never inherit hook stdin/stdout or hold its pipe open.
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .arg("--home")
        .arg(home)
        .args(["bridge", "start", "--foreground"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(port) = tcp {
        command.arg("--tcp").arg(port.to_string());
    }
    let mut child = command.spawn()?;
    for _ in 0..50 {
        if client.request(AgentEvent::Status).await.is_ok() {
            // Reap it if this process remains alive; the detached daemon owns its lifetime.
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Ok(());
        }
        if child.try_wait()?.is_some() {
            return Err(invalid(
                "Bridge failed to start; run bridge start --foreground for diagnostics",
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
    Err(invalid("Bridge startup timeout"))
}
pub async fn execute(cli: &Cli, home: &Path, cancel: &Cancellation) -> Result<Value> {
    let mut client = BridgeClient::new(home);
    client.timeout = Duration::from_secs(5);
    match &cli.command {
        Commands::Bridge { command } => match command {
            BridgeCommand::Start { foreground, tcp } => {
                if *foreground {
                    transport::serve(home, *tcp, cancel).await?;
                } else {
                    start(home, *tcp).await?;
                }
                Ok(
                    json!({"status":if *foreground{"stopped"}else{"running"},"protocol":PROTOCOL_VERSION}),
                )
            }
            BridgeCommand::Status => Ok(client
                .request(AgentEvent::Status)
                .await
                .unwrap_or_else(|e| json!({"status":"unavailable","reason":e.to_string()}))),
            BridgeCommand::Stop => {
                let result = client.request(AgentEvent::Shutdown).await?;
                for _ in 0..100 {
                    if !home.join("run/bridge-endpoint.json").exists() {
                        return Ok(json!({"status":"stopped"}));
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Ok(result)
            }
            BridgeCommand::Sessions => client.request(AgentEvent::Sessions).await,
            BridgeCommand::Inspect { id } => {
                client
                    .request(AgentEvent::Inspect {
                        hardknock_session_id: id.clone(),
                    })
                    .await
            }
            BridgeCommand::Call => client.request(serde_json::from_value(input()?)?).await,
        },
        Commands::IntegrationEvent {
            agent: HookAgent::Claude,
        } => {
            let payload = match input() {
                Ok(value) => value,
                Err(_) => {
                    eprintln!("Hardknock advisory unavailable: malformed hook payload");
                    return Ok(json!({}));
                }
            };
            // Experience hooks fail open with an explicit diagnostic; native permission checks remain.
            match integrations::claude::handle(home, payload).await {
                Ok(value) => Ok(value),
                Err(error) => {
                    eprintln!(
                        "Hardknock advisory unavailable: {}",
                        crate::bridge::privacy::redact(&error.to_string(), 256)
                    );
                    Ok(json!({}))
                }
            }
        }
        Commands::Agent { .. } => Ok(integrations::capabilities()),
        Commands::Integrate { command } => match command {
            IntegrationCommand::List | IntegrationCommand::Doctor => {
                integrations::status(home, matches!(command, IntegrationCommand::Doctor)).await
            }
            IntegrationCommand::Claude { command } => {
                integrations::install::manage("claude", home, command)
            }
            IntegrationCommand::Hermes { command } => {
                integrations::install::manage("hermes", home, command)
            }
            IntegrationCommand::Openclaw { command } => {
                integrations::install::manage("openclaw", home, command)
            }
            IntegrationCommand::Codex { command } => match command {
                CodexCommand::Check {
                    executable,
                    allow_untested,
                } => Ok(serde_json::to_value(
                    integrations::codex::check(executable, *allow_untested).await?,
                )?),
                CodexCommand::Run {
                    executable,
                    allow_untested,
                    resume,
                    model,
                    timeout_secs,
                    task,
                } => {
                    if ensure_started(home).await.is_err() {
                        eprintln!(
                            "Hardknock Bridge unavailable; continuing with native Codex permissions and no guaranteed recording"
                        );
                    }
                    integrations::codex::run(
                        home,
                        &cli.repo,
                        integrations::codex::RunOptions {
                            executable,
                            allow_untested: *allow_untested,
                            resume: resume.as_deref(),
                            model: model.as_deref(),
                            timeout: Duration::from_secs(*timeout_secs),
                            task,
                        },
                        cancel,
                    )
                    .await
                }
            },
        },
        Commands::Events {
            command: EventsCommand::Tail { follow, after },
        } => {
            let mut cursor = *after;
            loop {
                let response = client.request(AgentEvent::Events { after: cursor }).await?;
                if !follow {
                    return Ok(response);
                }
                for event in response["events"].as_array().into_iter().flatten() {
                    println!("{event}");
                    cursor = event["sequence"].as_u64().unwrap_or(cursor);
                }
                tokio::select! {_=cancel.cancelled()=>return Ok(json!({"stopped":true})),_=tokio::time::sleep(Duration::from_millis(300))=>{}}
            }
        }
        _ => Err(invalid("Not an integration command")),
    }
}
