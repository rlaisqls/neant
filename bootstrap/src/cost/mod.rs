//! Cost inference. `size` is the symbolic arithmetic, `analyze` the calculus, `lock` the file.

pub mod analyze;
pub mod assert;
pub mod bounds;
pub mod forest;
pub mod iolb;
pub mod lock;
pub mod measure;
pub mod piece;
pub mod rewrite;
pub mod scan;
pub mod scop;
pub mod size;

pub use analyze::{analyze, CostResult, FuncCost, Machine};
pub use piece::Cost;

/// An ablation: `NEANT_ABLATE=layout,region,reuse` turns off what one language decision buys, so
/// that what it buys can be counted (the compiler without it, on the same programs).
pub fn ablate(what: &str) -> bool {
    std::env::var("NEANT_ABLATE").is_ok_and(|v| v.split(',').any(|w| w == what))
}
