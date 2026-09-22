//! Structural edits for Rust dependency sources.
//!
//! ```
//! use r#override::{Edition, Source, item};
//!
//! let mut source = Source::parse("struct State { value: u32 }", Edition::Edition2024)?;
//! source.select(item("State").field("value"))?.set_visibility("pub(crate)")?;
//! assert_eq!(source.to_string(), "struct State { pub(crate) value: u32 }");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod edit;
mod fragment;
mod resolve;
mod select;
mod source;

pub use ra_ap_syntax::Edition;
pub use select::{Selector, arm, call, item, root};
pub use source::{Declaration, Selected, Source};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
