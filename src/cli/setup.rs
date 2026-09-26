// SPDX-License-Identifier: Apache-2.0

use clap::{Args, ValueEnum};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum SetupAgent {
    Auto,
    None,
    Claude,
    Codex,
    Hermes,
    Openclaw,
}

impl SetupAgent {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Hermes => "hermes",
            Self::Openclaw => "openclaw",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum SetupMode {
    Workstation,
    Ci,
}

#[derive(Clone, Debug, Args)]
pub struct SetupArgs {
    /// Detect supported local agents, or select one or more explicit adapters.
    #[arg(long, value_enum, default_value = "auto")]
    pub agent: Vec<SetupAgent>,
    /// Workstation installs a user service; CI keeps the Bridge on demand.
    #[arg(long, value_enum, default_value = "workstation")]
    pub mode: SetupMode,
    /// Never prompt. Setup is currently noninteractive in either mode.
    #[arg(long)]
    pub non_interactive: bool,
    /// Produce the complete plan without changing files or starting services.
    #[arg(long)]
    pub dry_run: bool,
    /// Start the Bridge after applying the managed configuration.
    #[arg(long)]
    pub start: bool,
}

#[derive(Clone, Debug, Args)]
pub struct UninstallArgs {
    /// Produce the removal plan without changing files or stopping services.
    #[arg(long)]
    pub dry_run: bool,
    /// Also remove the verified managed data home.
    #[arg(long)]
    pub remove_data: bool,
    /// Never prompt. Uninstall is currently noninteractive in either mode.
    #[arg(long)]
    pub non_interactive: bool,
}
