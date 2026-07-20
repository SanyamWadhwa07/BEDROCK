//! Dirty-rectangle tracking lives in the `bedrock-gfx` crate: it is pure
//! rectangle algebra with no framebuffer or MMIO dependency, so it is built
//! `no_std` but host-buildable and covered by ordinary `cargo test`
//! (including a randomized test that every marked pixel stays covered).
//!
//! This module re-exports it so the rest of the kernel keeps referring to
//! `graphics::dirty_rect::{Rect, DirtyTracker}` as before.

// `MAX_DIRTY_RECTS` is part of the module's surface (it is the cap the
// fragmentation fallback is documented against) even though the kernel
// binary itself never names it.
#[allow(unused_imports)]
pub use bedrock_gfx::{DirtyTracker, Rect, MAX_DIRTY_RECTS};
