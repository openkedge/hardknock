//! Deny-by-default execution capabilities and isolated Reality providers.

use std::path::Path;

mod container;
mod credential;
mod model;
mod policy;
mod profiles;
mod proxy;
mod token;

pub mod benchmark;

pub use benchmark::*;
pub use container::*;
pub use credential::*;
pub use model::*;
pub use policy::*;
pub use profiles::*;
pub use proxy::*;
pub use token::*;

/// Docker and Podman `--mount` bind syntax is writable by default. `rw` is a
/// volume-option token, not a valid bare `--mount` field; read-only bind mounts
/// use the portable boolean `readonly` field.
pub(crate) fn container_bind_mount(source: &Path, target: &str, read_only: bool) -> String {
    let mut mount = format!("type=bind,src={},dst={target}", source.to_string_lossy());
    if read_only {
        mount.push_str(",readonly");
    }
    mount
}
