//! Dependency-light owner/composition tests, NOT a GPUI or window check.
#[path = "../../../src/composition.rs"]
pub mod composition;
#[path = "../../../src/editor_bridge.rs"]
pub mod editor_bridge;
#[path = "../../../src/editor_transport.rs"]
pub mod editor_transport;
#[allow(dead_code)]
#[path = "../../../src/font_geometry.rs"]
mod font_geometry;
#[path = "../../../src/ime_context.rs"]
pub mod ime_context;
#[allow(dead_code)] // Native-only methods are exercised by the real GPUI check.
#[path = "../../../src/input_policy.rs"]
mod input_policy;
#[allow(dead_code)] // This checker has no FrameView consumer.
#[path = "../../../src/owner_input.rs"]
pub mod owner_input;

#[path = "../../../src/platform_context.rs"]
pub mod platform_context;

#[allow(dead_code)]
#[path = "../../../src/ime_keys.rs"]
mod ime_keys;
#[path = "../../../src/platform_state.rs"]
pub mod platform_state;

#[path = "../../../src/evaluation_quit.rs"]
pub mod evaluation_quit;

#[path = "../../../src/key_provenance.rs"]
mod key_provenance;
