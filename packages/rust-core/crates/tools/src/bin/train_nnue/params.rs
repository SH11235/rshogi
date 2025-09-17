pub const DEFAULT_ACC_DIM: &str = "256";
pub const DEFAULT_RELU_CLIP: &str = "127";
pub const MAX_PREFETCH_BATCHES: usize = 1024;

pub const GAP_WEIGHT_DIVISOR: f32 = 50.0;
pub const BASELINE_MIN_EPS: f32 = 1e-3;
pub const SELECTIVE_DEPTH_WEIGHT: f32 = 0.8;
pub const NON_EXACT_BOUND_WEIGHT: f32 = 0.7;
pub const SELECTIVE_DEPTH_MARGIN: i32 = 6;

pub const PERCENTAGE_DIVISOR: f32 = 100.0;
pub const CP_TO_FLOAT_DIVISOR: f32 = 100.0;
pub const CP_CLAMP_LIMIT: f32 = 20.0;
pub const NANOSECONDS_PER_SECOND: f64 = 1e9;
pub const BYTES_PER_MB: usize = 1024 * 1024;
pub const KB_TO_MB_DIVISOR: f32 = 1024.0;

pub const LINE_BUFFER_CAPACITY: usize = 64 * 1024;

pub const ADAM_BETA1: f32 = 0.9;
pub const ADAM_BETA2: f32 = 0.999;
pub const ADAM_EPSILON: f32 = 1e-8;

pub const MIN_ELAPSED_TIME: f64 = 1e-6;

pub const QUANTIZATION_MIN: f32 = -128.0;
pub const QUANTIZATION_MAX: f32 = 127.0;
pub const QUANTIZATION_METADATA_SIZE: usize = 3 * 8 + 4;
pub const CLASSIC_V1_ARCH_ID: u32 = 0x7AF3_2F16;
pub const I8_QMAX: i32 = 127;
pub const I16_QMAX: i32 = 32767;
pub const CLASSIC_ACC_DIM: usize = 256;
pub const CLASSIC_H1_DIM: usize = 32;
pub const CLASSIC_H2_DIM: usize = 32;
pub const CLASSIC_FT_SHIFT: i32 = 6;
pub const CLASSIC_RELU_CLIP: i32 = 127;
pub const CLASSIC_RELU_CLIP_F32: f32 = 127.0;
