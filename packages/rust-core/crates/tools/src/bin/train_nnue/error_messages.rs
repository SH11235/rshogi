// Common error messages for train_nnue

/// Error message for Classic architecture not supporting per-channel quantization on output layer
pub const ERR_CLASSIC_OUT_PER_CHANNEL: &str = "Classic v1: output layer does not support per-channel quantization; use --quant-out=per-tensor (default)";

/// Error message for Classic architecture not supporting per-channel quantization on feature transformer
pub const ERR_CLASSIC_FT_PER_CHANNEL: &str = "Classic v1: feature transformer does not support per-channel quantization; use --quant-ft=per-tensor (default)";

/// Error message for Classic export requiring distillation teacher
pub const ERR_CLASSIC_NEEDS_TEACHER: &str = "Classic export requires --distill-from-single";

/// Error message when classic-v1 export with stream-cache would skip distillation
pub const ERR_CLASSIC_STREAM_NEEDS_DISTILL: &str =
    "Classic v1 export requires distillation, but --stream-cache skips the distill pass. Run with --distill-only or disable --stream-cache.";

/// Error message for Single architecture not supporting Classic v1 format
pub const ERR_SINGLE_NO_CLASSIC_V1: &str =
    "--arch=single does not support --export-format classic-v1";

/// Error message for Classic architecture not supporting Single i8 format
pub const ERR_CLASSIC_NO_SINGLE_I8: &str =
    "--arch=classic does not support --export-format single-i8";
