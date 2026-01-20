pub mod eval_hash;
pub mod material;

pub use eval_hash::{eval_hash_enabled, set_eval_hash_enabled, EvalHash};
#[cfg(feature = "diagnostics")]
pub use eval_hash::{eval_hash_stats, reset_eval_hash_stats, EvalHashStats};
pub use material::{
    evaluate_pass_rights, get_material_level, get_pass_right_value, set_material_level,
    set_pass_right_value, set_pass_right_value_phased, MaterialLevel,
};
