// SPDX-License-Identifier: Apache-2.0
//! Explicit, deterministic operational knowledge. No runtime or enforcement coupling.
mod compatibility;
mod model;
mod resolution;
mod scope;
mod validation;
pub use model::*;
pub use resolution::*;
pub use scope::*;
pub use validation::*;
