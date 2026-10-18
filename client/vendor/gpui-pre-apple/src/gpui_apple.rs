#![cfg(target_os = "macos")]
// Ciranda's copy: upstream still uses the cocoa crate's deprecated types, which only warned while
// this crate came from crates.io with its lints capped.
#![allow(deprecated)]
//! Shared Apple platform support for GPUI.
//!
//! This crate contains the Metal renderer and GPU resource management shared
//! by GPUI's Apple platform backends.

mod metal_atlas;
pub mod metal_renderer;
