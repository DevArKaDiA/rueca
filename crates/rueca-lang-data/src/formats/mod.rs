//! Format front ends: mapping-shaped formats produce a [`crate::tree::Tree`];
//! HCL, Dockerfile and XML emit facts directly.

#[cfg(feature = "dockerfile")]
pub(crate) mod dockerfile;
#[cfg(feature = "hcl")]
pub(crate) mod hcl;
#[cfg(feature = "json")]
pub(crate) mod json;
pub(crate) mod lines;
#[cfg(feature = "toml")]
pub(crate) mod toml;
#[cfg(feature = "xml")]
pub(crate) mod xml;
#[cfg(feature = "yaml")]
pub(crate) mod yaml;
