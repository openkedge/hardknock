//! Execution substrate. Experiences, evaluators, and lesson inference come later.
#![cfg(unix)]

pub mod agent;
pub mod cli;
pub mod core;
pub mod dojo;
pub mod error;
pub mod process;
pub mod store;

pub use error::{Error, Result};
