// SPDX-License-Identifier: Apache-2.0

#[cfg(not(unix))]
compile_error!("Hardknock V0.1 currently supports Linux and macOS only.");

use std::{
    io::{self, Write},
    process::ExitCode,
};

use clap::Parser;
use hardknock::{
    Error,
    cancellation::Cancellation,
    cli::{Cli, execute},
};
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

fn report(json: bool, message: &str, code: u8) {
    let mut stderr = io::stderr().lock();
    let result = if json {
        writeln!(
            stderr,
            "{}",
            serde_json::json!({"event":"error", "message":message, "exit_code":code})
        )
    } else {
        writeln!(stderr, "hardknock: {message}")
    };
    let _ = result; // A closed diagnostic pipe cannot be reported through itself.
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                return if error.print().is_ok() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(2)
                };
            }
            report(
                std::env::args_os().any(|a| a == "--json"),
                &error.to_string(),
                2,
            );
            return ExitCode::from(2);
        }
    };
    let result = async {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(if cli.verbose {
                "hardknock=debug"
            } else {
                "warn"
            })
        });
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(io::stderr);
        if cli.json {
            subscriber.json().try_init()
        } else {
            subscriber.try_init()
        }
        .map_err(|e| Error::InvalidInput(format!("Cannot initialize tracing: {e}")))?;
        // Register before creating a worktree so SIGINT cannot skip cleanup setup.
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let cancel = Cancellation::default();
        let listener_cancel = cancel.clone();
        let listener = tokio::spawn(async move {
            tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {} }
            listener_cancel.cancel();
        });
        let result = execute(&cli, &cancel).await;
        listener.abort();
        let response = result?;
        response.print(&cli)?;
        Ok::<_, Error>(response.exit_code())
    }
    .await;
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let code = error.exit_code();
            report(cli.json, &error.to_string(), code);
            ExitCode::from(code)
        }
    }
}
