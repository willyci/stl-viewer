//! Geometry model, file parsers and shared helpers for STL Viewer.
//!
//! This crate has no GUI dependencies so it can be unit-tested headlessly.

pub mod files;
pub mod format;
pub mod mesh;
pub mod model;
pub mod palette;
mod par;
pub mod parsers;
pub mod xml;

pub use files::FileKind;
pub use model::ModelData;
pub use parsers::{LoadError, load_model};
