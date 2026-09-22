//! Cost inference. `size` is the symbolic arithmetic, `analyze` the calculus, `lock` the file.

pub mod analyze;
pub mod lock;
pub mod size;

pub use analyze::{analyze, CostResult, FuncCost, Machine};
