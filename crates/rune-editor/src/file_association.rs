//! Native per-buffer path identity. No filesystem operations or Lisp values.
use std::{path::Path, sync::Arc};
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileAssociation {
    pub(crate) revision: u64,
    pub(crate) path: Option<Arc<Path>>,
}
impl FileAssociation {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}
