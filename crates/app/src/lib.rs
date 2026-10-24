//! Library surface of the `irondict` front-end crate.
//!
//! The GUI is exposed here (rather than only from the binary) so the same code
//! can be linked by the Android `cdylib` entry point; the CLI binary imports it
//! back through the crate name.

pub mod gui;
