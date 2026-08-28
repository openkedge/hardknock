// SPDX-License-Identifier: Apache-2.0

//! Local execution, immutable evidence, and controlled empirical learning.
#![cfg(unix)]

pub mod agent;
pub mod application;
pub mod cancellation;
pub mod cli;
pub mod core;
pub mod dojo;
pub mod error;
pub mod evaluation;
pub mod experience;
pub mod experiment;
pub mod explanation;
pub mod learning_loop;
pub mod lesson;
pub mod perturbation;
pub mod process;
pub mod reflection;
pub mod resilience;
pub mod retrieval;
pub mod store;
pub mod validation;
pub mod workflow;

pub use error::{Error, Result};
