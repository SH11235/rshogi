//! LayerStack NNUE 統合テスト

use super::*;
use crate::position::{Position, SFEN_HIRATE};

/// バケット計算の境界値テスト
#[test]
fn test_bucket_boundaries() {
    let mut pos = Position::new();

    // 平手初期局面（両玉が端に配置）
    pos.set_sfen(SFEN_HIRATE).unwrap();

    // 2x2 バケット
    let bucket_2x2 = bucket_index(&pos, BucketDivision::TwoByTwo);
    assert!(bucket_2x2 < 4);

    // 3x3 バケット
    let bucket_3x3 = bucket_index(&pos, BucketDivision::ThreeByThree);
    assert!(bucket_3x3 < 9);
}

/// Product Pooling の出力範囲テスト
#[test]
fn test_product_pooling_range() {
    // 最大入力（127）
    let l0_max = [127u8; PERSPECTIVE_CAT];
    let mut x = [0u8; PP_OUT];
    product_pooling(&l0_max, &mut x);

    for &val in x.iter() {
        assert!(val <= 126, "PP output exceeds 126: {val}");
    }

    // 最小入力（0）
    let l0_min = [0u8; PERSPECTIVE_CAT];
    product_pooling(&l0_min, &mut x);

    for &val in x.iter() {
        assert_eq!(val, 0, "PP output should be 0 for zero input");
    }
}

/// Dual Activation の出力範囲テスト
#[test]
fn test_dual_activation_range() {
    // 大きな正の入力
    let l1_positive = [100000i32; L1_MAIN];
    let act = dual_activation(&l1_positive);

    for &val in act.iter() {
        assert!(val <= 127, "Dual activation output exceeds 127: {val}");
    }

    // 大きな負の入力
    let l1_negative = [-100000i32; L1_MAIN];
    let act = dual_activation(&l1_negative);

    // SqrCReLU: 負の入力も二乗するので正になる可能性
    for val in act.iter().take(L1_MAIN) {
        assert!(*val <= 127);
    }

    // CReLU: 負の入力は 0 にクランプ
    for val in act.iter().skip(L1_MAIN) {
        assert_eq!(*val, 0, "CReLU of negative should be 0");
    }
}

/// L2 の出力範囲テスト
#[test]
fn test_l2_output_range() {
    let act = [127u8; DUAL_ACT_OUT];
    let l2_weights = super::weights::L2WeightsBucket::new();

    let l2_out = layer_stack_l2(&act, &l2_weights);

    for &val in l2_out.iter() {
        assert!(val <= 127, "L2 output exceeds 127: {val}");
    }
}

/// Forward 全体の疎通テスト
#[test]
fn test_layer_stack_forward_smoke() {
    let x = [64u8; PP_OUT]; // 中間値
    let weights = LayerStackWeights::new(BucketDivision::TwoByTwo, true);

    // 全バケットで実行
    for bucket in 0..4 {
        let result = layer_stack_forward(&x, bucket, &weights);
        // ゼロ重みなので結果は 0
        assert_eq!(result, 0, "Zero weights should produce zero output");
    }
}

/// cp 変換テスト
#[test]
fn test_internal_to_cp_conversion() {
    // 基準: 127 内部スコア = 600 cp
    let cp = internal_to_cp(127);
    assert_eq!(cp, 600);

    // ゼロ
    assert_eq!(internal_to_cp(0), 0);

    // 負の値
    assert_eq!(internal_to_cp(-127), -600);
}

/// SqrCReLU の詳細テスト
#[test]
fn test_sqrcrelu_detail() {
    // x = 8192 (2^13) の場合
    // x² = 2^26
    // x² >> 19 = 2^7 = 128 → clamp to 127
    let l1 = [8192i32; L1_MAIN];
    let act = dual_activation(&l1);

    for val in act.iter().take(L1_MAIN) {
        assert_eq!(*val, 127, "SqrCReLU should saturate at 127");
    }

    // x = 724 の場合
    // x² = 524176
    // x² >> 19 = 0.999... → 0
    // 実際: 724² = 524,176, 524,176 >> 19 = 0.999... → 0
    let l1_small = [724i32; L1_MAIN];
    let act_small = dual_activation(&l1_small);
    // 724² = 524,176, >> 19 = 0
    assert_eq!(act_small[0], 0);

    // x = 725 の場合
    // 725² = 525,625, >> 19 = 1.001... → 1
    let l1_edge = [725i32; L1_MAIN];
    let act_edge = dual_activation(&l1_edge);
    assert_eq!(act_edge[0], 1);
}

/// CReLU の詳細テスト
#[test]
fn test_crelu_detail() {
    // x = 64 の場合
    // x >> 6 = 1
    let l1 = [64i32; L1_MAIN];
    let act = dual_activation(&l1);

    for val in act.iter().skip(L1_MAIN) {
        assert_eq!(*val, 1, "CReLU(64) should be 1");
    }

    // x = 127 * 64 = 8128 の場合
    // x >> 6 = 127
    let l1_max = [8128i32; L1_MAIN];
    let act_max = dual_activation(&l1_max);

    for val in act_max.iter().skip(L1_MAIN) {
        assert_eq!(*val, 127, "CReLU(8128) should be 127");
    }

    // x = 8192 の場合（飽和）
    // x >> 6 = 128 → clamp to 127
    let l1_sat = [8192i32; L1_MAIN];
    let act_sat = dual_activation(&l1_sat);

    for val in act_sat.iter().skip(L1_MAIN) {
        assert_eq!(*val, 127, "CReLU should saturate at 127");
    }
}

/// bypass の効果テスト
#[test]
fn test_bypass_effect() {
    let l2 = [0u8; L2_OUT];
    let bypass = 160; // / 16 = 10
    let mut out_weights = super::weights::OutWeightsBucket::new();
    out_weights.bias = 0;

    // bypass 有効
    let with_bypass = layer_stack_output(&l2, bypass, &out_weights, true);
    assert_eq!(with_bypass, 10);

    // bypass 無効
    let without_bypass = layer_stack_output(&l2, bypass, &out_weights, false);
    assert_eq!(without_bypass, 0);
}
