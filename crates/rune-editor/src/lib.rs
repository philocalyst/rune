#![forbid(unsafe_code)]
//! Headless session ownership, typed command dispatch and staged window data.
//! Positions are zero-based Unicode scalar offsets, never bytes or Lisp points.
mod document;
mod session;
mod windows;

pub use document::*;
pub use session::*;
pub use windows::*;

macro_rules! id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);
        impl $name {
            pub fn as_u64(self) -> u64 {
                self.0
            }
        }
    };
}
id!(BufferId);
id!(WindowId);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorError {
    SearchFailed,
    StaleInput,
    InvalidBuffer,
    InvalidWindow,
    InvalidPosition,
    InvalidRange,
    InvalidName,
    NameInUse,
    ReadOnly,
    ModifiedBuffer,
    LastWindow,
    InvalidGeometry,
    IdExhausted,
    NoUndo,
    NoRedo,
    ForeignConfiguration,
    UnboundKey,
    InvalidBinding,
    MacroState,
    MacroTooLarge,
    Backend(String),
    CommandPanicked,
}
impl std::fmt::Display for EditorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for EditorError {}
pub type Result<T> = std::result::Result<T, EditorError>;

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
pub(crate) fn allocate_id() -> Result<u64> {
    NEXT_ID
        .fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |id| id.checked_add(1),
        )
        .map_err(|_| EditorError::IdExhausted)
}

#[cfg(feature = "rope")]
mod rope_document;
#[cfg(feature = "rope")]
pub use rope_document::*;

mod boundary;
pub use boundary::{from_lisp_point, to_lisp_point};
