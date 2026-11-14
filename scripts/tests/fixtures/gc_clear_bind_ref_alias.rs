// Negative crate-module compile fixture. Expected E0502: the outer reference
// returned by bind_ref borrows the root, but its inner heap lifetime still
// borrows Context and must block exclusive string erasure.
use crate::core::{
    gc::{Context, Rto},
    object::{Gc, LispString},
};
fn forbidden_bind_ref_alias(string: &Rto<Gc<&LispString>>, cx: &mut Context) {
    let bound = string.bind_ref(cx);
    let borrowed = bound.untag().inner();
    LispString::clear_rooted(string, cx).unwrap();
    std::hint::black_box(borrowed);
}
