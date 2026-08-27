//! Typed substrate records and local persistence.
#![cfg(unix)]

pub mod core;
pub mod dojo;
pub mod error;
pub mod store;

pub use error::{Error, Result};
