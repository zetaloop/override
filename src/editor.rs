mod edit;
pub(crate) mod flow;
pub(crate) mod fragment;
pub(crate) mod region;
pub(crate) mod resolve;
pub(crate) mod select;
pub(crate) mod source;
pub(crate) mod symbol;

#[cfg(feature = "build")]
pub(crate) mod package;
#[cfg(feature = "build")]
pub(crate) mod sources;

#[cfg(feature = "build")]
pub use cargo_config2::Config;
#[cfg(feature = "build")]
pub use cargo_metadata::{DependencyKind, Target, TargetKind};
#[cfg(feature = "build")]
pub use package::Package;
#[cfg(feature = "build")]
pub use sources::Sources;
#[cfg(feature = "build")]
pub use toml::Table as Manifest;

pub use ra_ap_syntax::Edition;
pub use region::Boundary;
pub use select::{Parameter, Selector, arm, call, item, root};
pub use source::{Declaration, Selected, Source};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
