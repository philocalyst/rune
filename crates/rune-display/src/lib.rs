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

mod font_capabilities;
pub use font_capabilities::{MeasuredFontCapabilities, MeasuredFontVariant};

mod frame_font_plan;
pub use frame_font_plan::{FrameFontAssignment, FrameFontPlan};

mod grapheme_font_guard;
pub use grapheme_font_guard::{GraphemeFontRun, validate_grapheme_font_runs};

mod measured_grapheme_layout;
pub use measured_grapheme_layout::{MeasuredLayoutScope, WidthProvenance, ContextualClusterWidth, MeasuredGraphemeLayoutPlan, layout_measured_graphemes};
