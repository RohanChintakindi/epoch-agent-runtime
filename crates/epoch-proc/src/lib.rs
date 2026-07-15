//! Bounded, diagnostic-rich semantic collection from Linux procfs.

mod model;
mod parse;

pub use model::*;
pub use parse::{decode_capability_mask, parse_cgroups, parse_maps, parse_status};
