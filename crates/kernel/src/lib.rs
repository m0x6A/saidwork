//! Shared kernel for the saidwork modular monolith.
//!
//! Deliberately tiny: only the primitives that every vertical slice needs and
//! that carry no business rules of their own. Each slice owns its domain model.

mod clock;
mod paths;

pub use clock::{Clock, FixedClock, SystemClock};
pub use paths::{AppPaths, PathsError, expand_tilde};
pub use secrecy::{ExposeSecret, SecretString};
pub mod retry;
pub mod urls;
