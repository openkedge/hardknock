// SPDX-License-Identifier: Apache-2.0

use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// Serve the bounded Hardknock tool surface over newline-delimited stdio.
    Serve(McpServeArgs),
}

#[derive(Debug, Args)]
pub struct McpServeArgs {
    /// Use the MCP stdio transport.
    #[arg(long, required = true)]
    pub stdio: bool,
    /// Bind context and trusted evaluators to this existing workspace.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
}
