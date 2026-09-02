// SPDX-License-Identifier: Apache-2.0

//! Diversity-aware empirical evidence coordination.
//!
//! The dependency graph records observable, known dependencies which may
//! create correlated failures. It does not prove statistical independence or
//! causality. Unknown dependency metadata never earns diversity credit.

mod model;
mod policy;

pub use model::*;
pub use policy::*;

pub const DIVERSITY_POLICY_VERSION: &str = "hardknock.epistemic-diversity.v1";
pub const FUSION_POLICY_VERSION: &str = "hardknock.evidence-fusion.v1";
