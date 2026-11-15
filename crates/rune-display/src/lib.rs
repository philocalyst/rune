#![forbid(unsafe_code)]
//! Immutable, interpreter-free first-light display pipeline.
mod layout;
mod protocol;
mod snapshot;
pub mod svg;
mod transport;
mod row_activity;
/// Dormant bounded nonblocking row preparation handoff.
pub mod retained_row_handoff;
/// Dormant bounded correlation only; no producer or native renderer activation.
pub mod retained_row_transaction;
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

/// Experimental structural admission; no actual GPUI coverage authority.
pub mod row_shape_admission;
