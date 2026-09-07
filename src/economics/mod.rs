// SPDX-License-Identifier: Apache-2.0
//! Deterministic, explainable allocation of bounded experience-acquisition budgets.
mod engine;
mod model;

pub mod benchmark;
pub use engine::*;
pub use model::*;
