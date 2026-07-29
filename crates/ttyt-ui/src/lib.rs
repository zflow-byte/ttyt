#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod app;
pub mod terminal;
pub mod theme;
pub mod widgets;

pub use app::{App, run, spawn_input_thread};
pub use theme::Theme;
