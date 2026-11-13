#![forbid(unsafe_code)]
//! Immutable, interpreter-free first-light display pipeline.
mod layout;
mod protocol;
mod snapshot;
pub mod svg;
mod transport;
pub use layout::*;
pub use protocol::*;
pub use snapshot::*;
pub use transport::*;

#[cfg(feature = "rope")]
mod rope_snapshot;
#[cfg(feature = "rope")]
pub use rope_snapshot::RopeSnapshot;

mod host_wake;
pub use host_wake::*;

pub mod first_light_host;
