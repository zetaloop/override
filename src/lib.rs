mod edit;
mod flow;
mod fragment;
mod region;
mod resolve;
mod select;
mod source;

#[cfg(feature = "build")]
mod manifest;
#[cfg(feature = "build")]
mod package;
#[cfg(feature = "build")]
mod sources;

#[cfg(feature = "build")]
pub use cargo_metadata::{DependencyKind, Target, TargetKind};
#[cfg(feature = "build")]
pub use manifest::check_target;
#[cfg(feature = "build")]
pub use package::Package;
#[cfg(feature = "build")]
pub use sources::{Entry, Sources};

pub use ra_ap_syntax::Edition;
pub use region::Boundary;
pub use select::{Selector, arm, call, item, root};
pub use source::{Declaration, Selected, Source};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
