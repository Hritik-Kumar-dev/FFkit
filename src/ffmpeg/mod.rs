//! ffmpeg integration: subprocess-based, never linked (spec section 0).
//!
//! M1 declares the module surface; real implementations land in M2 (probe,
//! capabilities), M3 (builder), and M4 (runner, progress, errors).

pub mod builder;
pub mod capabilities;
pub mod errors;
pub mod probe;
pub mod progress;
pub mod runner;
