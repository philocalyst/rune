//! Pure Rust persistent text. I2 draft: zero-based scalar coordinates, immutable
//! snapshots, and bounded chunk/character readers. No Lisp heap pointers.
//!
//! This first layer accepts Unicode UTF-8 only. Emacs raw-byte characters,
//! coding systems and Lisp property-value ownership require separate adapters.
#![forbid(unsafe_code)]
mod rope;
pub use rope::*;
mod document;
mod properties;
pub use document::*;
pub use properties::*;
mod positions;
pub use positions::*;

mod marker_undo;
pub use marker_undo::*;
