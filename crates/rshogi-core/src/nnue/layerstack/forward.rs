//! LayerStack Forward Pass
//!
//! bit-exact 互換の基準となる Canonical Integer Forward 実装。
//! Golden Forward テストは `x: [u8; 1536]`（Product Pooling 出力）以降で行う。

use super::constants::*;
use super::weights::{L1WeightsBucket, L2WeightsBucket, LayerStackWeights, OutWeightsBucket};

// =============================================================================
// Product Pooling
// =============================================================================

/// Product Pooling
///
/// 3072次元を4分割し、前半同士・後半同士を要素積で結合して1536次元に削減。
///
/// # レイアウト
///
/// ```text
/// 入力 l0[3072]:
///   [0..768]     = STM_FT の前半
///   [768..1536]  = STM_FT の後半
///   [1536..2304] = NSTM_FT の前半
///   [2304..3072] = NSTM_FT の後半
///
/// 出力 x[1536]:
///   x[0..768]    = l0[0..768] * l0[768..1536]      (STM側の積)
///   x[768..1536] = l0[1536..2304] * l0[2304..3072] (NSTM側の積)
/// ```
///
/// # スケーリング
///
/// `(a * b) >> 7` で [0, 127] × [0, 127] → [0, 126] に正規化
/// `/127` を `/128` で近似
#[inline]
pub fn product_pooling(l0: &[u8; PERSPECTIVE_CAT], x: &mut [u8; PP_OUT]) {
    // STM 側の積: l0[0..768] * l0[768..1536]
    for i in 0..768 {
        let a = l0[i] as u16;
        let b = l0[i + 768] as u16;
        x[i] = ((a * b) >> PP_SHIFT).min(PP_MAX_OUT as u16) as u8;
    }

    // NSTM 側の積: l0[1536..2304] * l0[2304..3072]
    for i in 0..768 {
        let a = l0[i + 1536] as u16;
        let b = l0[i + 2304] as u16;
        x[i + 768] = ((a * b) >> PP_SHIFT).min(PP_MAX_OUT as u16) as u8;
    }
}

// =============================================================================
// L1 層
// =============================================================================

/// L1 層の順伝播
///
/// 入力 1536 → 出力 16（main 15 + bypass 1）
///
/// # 戻り値
///
/// `(l1_main, l1_bypass)`: main は Dual Activation への入力、bypass は Output で加算
///
/// # 注意
///
/// Factorizer は export 時に統合済みのため、ここでは単純な積和演算のみ。
#[inline]
pub fn layer_stack_l1(x: &[u8; L1_IN], w: &L1WeightsBucket) -> ([i32; L1_MAIN], i32) {
    let mut out = [0i32; L1_OUT];

    // バイアスで初期化
    out.copy_from_slice(&*w.bias);

    // 積和演算
    for (j, &x_val) in x.iter().enumerate() {
        let x_val = x_val as i32;
        for (i, out_val) in out.iter_mut().enumerate() {
            *out_val += x_val * w.weight[i * L1_IN + j] as i32;
        }
    }

    // main と bypass に分割
    let mut l1_main = [0i32; L1_MAIN];
    l1_main.copy_from_slice(&out[0..L1_MAIN]);
    let l1_bypass = out[L1_MAIN]; // out[15]

    (l1_main, l1_bypass)
}

// =============================================================================
// Dual Activation
// =============================================================================

/// Dual Activation
///
/// SqrCReLU と CReLU を並列に適用し、15 → 30 に次元を倍増。
///
/// # 出力レイアウト
///
/// ```text
/// out[0..15]  = SqrCReLU(l1[0..15])  // x² >> 19, clamp [0, 127]
/// out[15..30] = CReLU(l1[0..15])     // x >> 6, clamp [0, 127]
/// ```
#[inline]
pub fn dual_activation(l1: &[i32; L1_MAIN]) -> [u8; DUAL_ACT_OUT] {
    let mut out = [0u8; DUAL_ACT_OUT];

    for i in 0..L1_MAIN {
        let val = l1[i];

        // SqrCReLU: x² >> 19, clamp [0, 127]
        let squared = (val as i64 * val as i64) >> SQRCRELU_SHIFT;
        out[i] = squared.clamp(0, ACT_MAX_OUT as i64) as u8;

        // CReLU: x >> 6, clamp [0, 127]
        let shifted = val >> CRELU_SHIFT;
        out[i + L1_MAIN] = shifted.clamp(0, ACT_MAX_OUT) as u8;
    }

    out
}

// =============================================================================
// L2 層
// =============================================================================

/// L2 層の順伝播
///
/// 入力 30 → 出力 64
///
/// 出力は `>> 6` してから [0, 127] にクランプ
#[inline]
pub fn layer_stack_l2(act: &[u8; DUAL_ACT_OUT], w: &L2WeightsBucket) -> [u8; L2_OUT] {
    let mut out = [0u8; L2_OUT];

    for (i, out_val) in out.iter_mut().enumerate() {
        let mut acc = w.bias[i];

        for (j, &act_val) in act.iter().enumerate() {
            acc += act_val as i32 * w.weight[i * DUAL_ACT_OUT + j] as i32;
        }

        *out_val = (acc >> L2_SHIFT).clamp(0, ACT_MAX_OUT) as u8;
    }

    out
}

// =============================================================================
// Output 層
// =============================================================================

/// Output 層の順伝播
///
/// 入力 64 → 出力 1（内部スコア）
///
/// bypass が有効な場合は L1 の bypass 出力を加算
#[inline]
pub fn layer_stack_output(
    l2: &[u8; L2_OUT],
    bypass: i32,
    w: &OutWeightsBucket,
    use_bypass: bool,
) -> i32 {
    let mut acc = w.bias;

    for (l2_val, &w_val) in l2.iter().zip(w.weight.iter()) {
        acc += *l2_val as i32 * w_val as i32;
    }

    if use_bypass {
        acc += bypass;
    }

    acc / WEIGHT_SCALE_OUT
}

// =============================================================================
// 統合 Forward（Product Pooling 以降）
// =============================================================================

/// LayerStack の forward pass（Product Pooling 出力以降）
///
/// # 引数
///
/// - `x`: Product Pooling 出力 [u8; 1536]
/// - `bucket`: バケットインデックス
/// - `weights`: LayerStack の重み
///
/// # 戻り値
///
/// 内部スコア（cp 変換前）
#[inline]
pub fn layer_stack_forward(x: &[u8; PP_OUT], bucket: usize, weights: &LayerStackWeights) -> i32 {
    // L1
    let (l1_main, l1_bypass) = layer_stack_l1(x, &weights.l1[bucket]);

    // Dual Activation
    let act = dual_activation(&l1_main);

    // L2
    let l2 = layer_stack_l2(&act, &weights.l2[bucket]);

    // Output + Bypass
    layer_stack_output(&l2, l1_bypass, &weights.out[bucket], weights.use_bypass)
}

/// 内部スコアを cp（centi-pawn）に変換
///
/// `internal * NNUE2SCORE / QUANTIZED_ONE`
#[inline]
pub fn internal_to_cp(internal: i32) -> i32 {
    internal * NNUE2SCORE / QUANTIZED_ONE as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::layerstack::bucket::BucketDivision;

    #[test]
    fn test_product_pooling_output_range() {
        // 最大入力（127 * 127 >> 7 = 126）
        let mut l0 = [127u8; PERSPECTIVE_CAT];
        let mut x = [0u8; PP_OUT];

        product_pooling(&l0, &mut x);

        // 全要素が [0, 126] に収まることを確認
        for &val in x.iter() {
            assert!(val <= PP_MAX_OUT);
        }

        // 具体的な値を確認
        // 127 * 127 = 16129, 16129 >> 7 = 126.0078... → 126
        assert_eq!(x[0], 126);
        assert_eq!(x[767], 126);
        assert_eq!(x[768], 126);
        assert_eq!(x[1535], 126);

        // ゼロ入力
        l0 = [0u8; PERSPECTIVE_CAT];
        product_pooling(&l0, &mut x);
        for &val in x.iter() {
            assert_eq!(val, 0);
        }
    }

    #[test]
    fn test_product_pooling_layout() {
        // STM 側と NSTM 側が正しく計算されることを確認
        let mut l0 = [0u8; PERSPECTIVE_CAT];

        // STM 前半: 10, STM 後半: 20
        for i in 0..768 {
            l0[i] = 10;
            l0[i + 768] = 20;
        }

        // NSTM 前半: 30, NSTM 後半: 40
        for i in 0..768 {
            l0[i + 1536] = 30;
            l0[i + 2304] = 40;
        }

        let mut x = [0u8; PP_OUT];
        product_pooling(&l0, &mut x);

        // STM: (10 * 20) >> 7 = 200 >> 7 = 1
        assert_eq!(x[0], 1);
        assert_eq!(x[767], 1);

        // NSTM: (30 * 40) >> 7 = 1200 >> 7 = 9
        assert_eq!(x[768], 9);
        assert_eq!(x[1535], 9);
    }

    #[test]
    fn test_dual_activation_ranges() {
        // ゼロ入力
        let l1_zero = [0i32; L1_MAIN];
        let act_zero = dual_activation(&l1_zero);

        for &val in act_zero.iter() {
            assert_eq!(val, 0);
        }

        // 正の入力
        let l1_positive: [i32; L1_MAIN] = [1000; L1_MAIN];
        let act_positive = dual_activation(&l1_positive);

        // SqrCReLU: 1000² >> 19 = 1000000 >> 19 ≈ 1.9 → clamp to 1
        // 実際: 1000000 / 524288 = 1.907... → 1
        for val in act_positive.iter().take(L1_MAIN) {
            assert!(*val <= 127);
        }

        // CReLU: 1000 >> 6 = 15
        for val in act_positive.iter().skip(L1_MAIN) {
            assert_eq!(*val, 15);
        }

        // 負の入力
        let l1_negative = [-1000i32; L1_MAIN];
        let act_negative = dual_activation(&l1_negative);

        // SqrCReLU: (-1000)² >> 19 = 1000000 >> 19 ≈ 1
        // 負の二乗なので正になる（u8 なので常に >= 0 だが、計算結果が期待通りか確認）
        for val in act_negative.iter().take(L1_MAIN) {
            // 1000000 >> 19 = 1.907... → 1
            assert_eq!(*val, 1);
        }

        // CReLU: -1000 >> 6 = -16 → clamp to 0
        for val in act_negative.iter().skip(L1_MAIN) {
            assert_eq!(*val, 0);
        }
    }

    #[test]
    fn test_l1_basic() {
        let mut x = [0u8; L1_IN];
        x[0] = 1;

        let mut l1_weights = super::super::weights::L1WeightsBucket::new();
        l1_weights.bias[0] = 100;
        l1_weights.weight[0] = 5; // weight[output=0][input=0]

        let (main, bypass) = layer_stack_l1(&x, &l1_weights);

        // out[0] = bias[0] + x[0] * weight[0] = 100 + 1 * 5 = 105
        assert_eq!(main[0], 105);

        // bypass = out[15]（バイアスのみ）
        assert_eq!(bypass, 0);
    }

    #[test]
    fn test_l2_clamp() {
        let act = [127u8; DUAL_ACT_OUT];
        let mut l2_weights = super::super::weights::L2WeightsBucket::new();

        // 大きなバイアスで飽和させる
        l2_weights.bias[0] = 100000;

        let l2_out = layer_stack_l2(&act, &l2_weights);

        // (100000 + ...) >> 6 → 127 にクランプ
        assert!(l2_out[0] <= 127);
    }

    #[test]
    fn test_output_bypass() {
        let l2 = [0u8; L2_OUT];
        let bypass = 1000;
        let mut out_weights = super::super::weights::OutWeightsBucket::new();
        out_weights.bias = 160; // / 16 = 10

        // bypass 有効
        let result_with_bypass = layer_stack_output(&l2, bypass, &out_weights, true);
        // (160 + 1000) / 16 = 72.5 → 72
        assert_eq!(result_with_bypass, 72);

        // bypass 無効
        let result_without_bypass = layer_stack_output(&l2, bypass, &out_weights, false);
        // 160 / 16 = 10
        assert_eq!(result_without_bypass, 10);
    }

    #[test]
    fn test_internal_to_cp() {
        // 1000 * 600 / 127 ≈ 4724
        assert_eq!(internal_to_cp(1000), 4724);

        // 0 * 600 / 127 = 0
        assert_eq!(internal_to_cp(0), 0);

        // -500 * 600 / 127 ≈ -2362
        assert_eq!(internal_to_cp(-500), -2362);
    }

    #[test]
    fn test_layer_stack_forward_zeros() {
        let x = [0u8; PP_OUT];
        let weights = LayerStackWeights::new(BucketDivision::TwoByTwo, true);

        // ゼロ入力、ゼロ重みなら出力もゼロ
        let result = layer_stack_forward(&x, 0, &weights);
        assert_eq!(result, 0);
    }
}
