// Negative crate-module compile fixture. Expected E0502. Compile by including
// this item in the rune crate when checking the exclusive string mutation API.
use crate::core::{
    gc::{Context, Rto},
    object::{Gc, LispString},
};
fn forbidden_live_alias(string: &Rto<Gc<&LispString>>, cx: &mut Context) {
    let borrowed = string.bind(cx).untag().inner();
    LispString::clear_rooted(string, cx).unwrap();
    std::hint::black_box(borrowed);
}
