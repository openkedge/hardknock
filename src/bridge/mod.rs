// SPDX-License-Identifier: Apache-2.0
//! Local vendor-neutral lifecycle boundary. Adapters may only use this API.
pub mod cache;
pub mod config;
pub mod engine;
mod experiments;
pub mod privacy;
pub mod protocol;
mod recording;
pub mod transport;
pub use engine::Bridge;
