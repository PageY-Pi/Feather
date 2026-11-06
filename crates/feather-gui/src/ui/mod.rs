//! The window's painted regions, split out of `main.rs`.
//!
//! `FeatherApp` itself stays in the crate root so every module here can reach
//! its fields directly; these modules only own the code that lays out and
//! paints one region of the frame. Each is an `impl FeatherApp` block, so the
//! split is purely a matter of where a method lives, not of what it can touch.
//!
//! Kept apart from `cards`/`transcript`, which own painting the transcript
//! itself: those are independently testable and know nothing about the app.

pub(crate) mod composer;
pub(crate) mod frame;
pub(crate) mod modals;
pub(crate) mod top_bar;
