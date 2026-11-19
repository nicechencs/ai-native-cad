pub mod checker;
pub mod ir;
pub mod types;

pub use checker::{CheckOutput, check_and_lower, classify_expr};
pub use ir::*;
pub use types::*;
