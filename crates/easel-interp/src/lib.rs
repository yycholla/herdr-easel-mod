//! Interprets a declarative subset of GPUI element code.
//!
//! [`interpret`] turns a snippet into an [`ir::Node`] tree without compiling
//! it. With the `render` feature, [`render::to_element`] turns that tree into
//! real gpui elements.

mod diagnostic;
mod interp;
pub mod ir;
pub mod styled;

pub use diagnostic::{Diagnostic, DiagnosticKind};
pub use interp::{Program, interpret};

#[cfg(feature = "render")]
pub mod render;
