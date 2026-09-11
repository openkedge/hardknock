// SPDX-License-Identifier: Apache-2.0
//! Shared operational knowledge, immutable audit state, and governance handoff.
mod conflict;
mod context;
mod guard;
mod health;
mod model;
mod resolution;
pub use conflict::*;
pub use context::*;
pub use guard::*;
pub use health::*;
pub use model::*;
pub use resolution::*;
pub const RESOLUTION_POLICY_VERSION: &str = "hardknock.hierarchy-runtime.v1";
