pub mod eval_hash;
pub mod material;

pub use eval_hash::{EvalHash, eval_hash_enabled, set_eval_hash_enabled};
#[cfg(feature = "diagnostics")]
pub use eval_hash::{EvalHashStats, eval_hash_stats, reset_eval_hash_stats};
pub use material::{
    DEFAULT_PASS_RIGHT_VALUE_EARLY, DEFAULT_PASS_RIGHT_VALUE_LATE, MaterialLevel,
    evaluate_pass_rights, get_material_level, get_pass_move_bonus, get_pass_right_value,
    get_scaled_pass_move_bonus, set_material_level, set_pass_move_bonus, set_pass_right_value,
    set_pass_right_value_phased,
};
