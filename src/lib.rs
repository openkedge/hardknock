// SPDX-License-Identifier: Apache-2.0

//! Local execution, immutable evidence, and controlled empirical learning.
#![cfg(unix)]

pub mod agent;
pub mod cancellation;
pub mod cli;
pub mod core;
pub mod dojo;
pub mod error;
pub mod evaluation;
pub mod experience;
pub mod process;
pub mod store;
pub mod workflow;

pub use error::{Error, Result};
