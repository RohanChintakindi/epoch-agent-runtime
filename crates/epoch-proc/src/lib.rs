//! Bounded, diagnostic-rich semantic collection from Linux procfs.

mod kernel;
mod model;
mod parse;

pub use kernel::{
    normalize_fd_target, parse_inet_table, parse_namespace_target, summarize_fd_targets,
};
pub use model::*;
pub use parse::{decode_capability_mask, parse_cgroups, parse_maps, parse_status};
