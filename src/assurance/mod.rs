// SPDX-License-Identifier: Apache-2.0

//! Revision-scoped behavioral contracts and empirical assurance.
//!
//! Assurance is deliberately evidence- and profile-relative. A certificate is
//! an assertion about one Skill revision under one contract and one profile;
//! it is never a claim of universal correctness.

mod artifact;
mod evaluator;
mod model;

pub use artifact::*;
pub use evaluator::*;
pub use model::*;

pub const CONTRACT_SCHEMA_V1: &str = "hardknock.contract.v1";
pub const CERTIFICATION_SCHEMA_V1: &str = "hardknock.certification.v1";
pub const CONTRACT_EVALUATOR_VERSION: &str = "hardknock.contract-evaluator.v1";
pub const EVIDENCE_POLICY_VERSION: &str = "hardknock.certification-evidence.v1";
pub const FRESHNESS_POLICY_VERSION: &str = "hardknock.certification-freshness.v1";
pub const CAPABILITY_POLICY_VERSION: &str = "hardknock.capability-assurance.v1";
