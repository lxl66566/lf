//! Core library for `lf`: detect text files and convert their line endings to
//! LF.
//!
//! The crate is split from the `lf` binary so that the detection and conversion
//! logic can be reused (and unit-tested) independently of the CLI.

#![warn(clippy::pedantic, clippy::cargo)]
#![allow(clippy::too_many_lines)]

pub mod convert;
pub mod detect;

pub use convert::{ConvertOptions, ConvertOutcome, convert_path};
pub use detect::ContentType;
