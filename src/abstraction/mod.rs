// SPDX-License-Identifier: Apache-2.0
//! Evidence-backed abstraction, held-out transfer, and bounded knowledge resolution.
mod engine;
mod model;

pub mod benchmark;
pub use engine::*;
pub use model::*;
