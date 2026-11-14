// Negative crate-module compile fixture. Expected E0133. Erased-lifetime root
// copies must require an explicit unsafe owner/GC synchronization proof.
use crate::core::{gc::Rto, object::Object};
fn forbidden_unbound_copy(root: &Rto<Object>) {
    let _escaped = root.get_inner();
}
