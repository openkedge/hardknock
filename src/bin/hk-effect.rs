// SPDX-License-Identifier: Apache-2.0

//! Minimal in-Reality effect client. It intentionally has no commit command.
//! A container image can include this binary and connect it to a dedicated
//! Hardknock Bridge relay at `/run/hardknock/bridge.sock`.

use clap::{Parser, Subcommand};
use hardknock::{
    Error, Result,
    bridge::protocol::{
        AgentEvent, BridgeEnvelope, BridgeResponse, MAX_EVENT_BYTES, PROTOCOL_VERSION,
    },
    capability::SignedRealityCapabilityToken,
    core::EffectId,
    effects::EffectRequest,
};
use std::{io::Write, path::PathBuf};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

#[derive(Parser)]
#[command(name = "hk-effect", version, about = "Scoped Hardknock Effect client")]
struct Cli {
    #[arg(long, default_value = "/run/hardknock/bridge.sock")]
    socket: PathBuf,
    #[arg(long, default_value = "/run/hardknock/capability-token.json")]
    token: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Propose { request: PathBuf },
    Status { effect: EffectId },
    Discard { effect: EffectId },
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run(Cli::parse()).await {
        Ok(value) => {
            let mut stdout = std::io::stdout().lock();
            if serde_json::to_writer_pretty(&mut stdout, &value).is_ok() && writeln!(stdout).is_ok()
            {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::from(2)
            }
        }
        Err(error) => {
            eprintln!("hk-effect: {error}");
            std::process::ExitCode::from(error.exit_code())
        }
    }
}

async fn run(cli: Cli) -> Result<serde_json::Value> {
    let token_data = bounded_file(&cli.token)?;
    let token: SignedRealityCapabilityToken = serde_json::from_slice(&token_data)?;
    let event = match cli.command {
        Command::Propose { request } => {
            let mut request: EffectRequest = serde_json::from_slice(&bounded_file(&request)?)?;
            request.reality_id = Some(token.claims.reality_id.clone());
            AgentEvent::RealityEffectProposed {
                reality_id: token.claims.reality_id.clone(),
                request,
            }
        }
        Command::Status { effect } => AgentEvent::RealityEffectStatus {
            reality_id: token.claims.reality_id.clone(),
            effect_id: effect,
        },
        Command::Discard { effect } => AgentEvent::RealityEffectDiscardRequested {
            reality_id: token.claims.reality_id.clone(),
            effect_id: effect,
        },
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let envelope = BridgeEnvelope {
        protocol_version: PROTOCOL_VERSION.into(),
        request_id: request_id.clone(),
        token: serde_json::to_string(&token)?,
        payload: event,
    };
    let mut bytes = serde_json::to_vec(&envelope)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_EVENT_BYTES {
        return Err(Error::InvalidInput("Effect request is too large".into()));
    }
    let stream = UnixStream::connect(&cli.socket).await?;
    let mut stream = BufReader::new(stream);
    stream.get_mut().write_all(&bytes).await?;
    let mut response = Vec::new();
    stream.read_until(b'\n', &mut response).await?;
    if response.len() > MAX_EVENT_BYTES {
        return Err(Error::InvalidInput("Bridge response is too large".into()));
    }
    let response: BridgeResponse = serde_json::from_slice(&response)?;
    if response.protocol_version != PROTOCOL_VERSION || response.request_id != request_id {
        return Err(Error::Intervention(
            "Bridge response correlation mismatch".into(),
        ));
    }
    if !response.ok {
        return Err(Error::Intervention(
            response
                .error
                .map(|error| format!("{}: {}", error.code, error.message))
                .unwrap_or_else(|| "Bridge rejected effect request".into()),
        ));
    }
    response
        .payload
        .ok_or_else(|| Error::InvalidInput("Bridge response payload missing".into()))
}

fn bounded_file(path: &std::path::Path) -> Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 256 * 1024 {
        return Err(Error::Intervention(
            "Effect client input must be a regular file of at most 256 KiB".into(),
        ));
    }
    Ok(std::fs::read(path)?)
}
