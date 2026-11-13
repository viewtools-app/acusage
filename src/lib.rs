/// Project homepage, shown in the title box and `--help`. The single place to change it;
/// Cargo.toml `homepage` must match.
#[macro_export]
macro_rules! homepage {
    () => {
        "https://acusage.viewtools.app"
    };
}

pub const HOMEPAGE: &str = homepage!();

pub mod app;
pub mod blocks;
pub mod cli;
pub mod config;
pub mod model;
pub mod pricing;
pub mod render;
pub mod report;
pub mod sources;
