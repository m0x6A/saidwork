//! Terminal user interface built on ratatui.
//!
//! Elm-style: [`model::Model`] is pure state, [`model::update`] turns a
//! [`model::Msg`] into a new state plus side-effect requests ([`model::Cmd`]),
//! [`view::render`] draws, and [`runtime`] runs the loop and executes commands.

pub mod model;
mod runtime;
pub mod view;

pub use runtime::{TuiError, run};
