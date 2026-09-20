//! tools の native 評価経路が係数ロードまで対応する routing mode の境界。
use anyhow::{Result, bail};
use rshogi_core::nnue::{LayerStackBucketMode, parse_layer_stack_bucket_mode};

/// tools の native 経路で実装済みの mode だけ受理する。
/// core の parser に mode が増えても係数未設定のまま評価へ進めない。
pub fn parse_native_layer_stack_bucket_mode(value: &str) -> Result<LayerStackBucketMode> {
    match parse_layer_stack_bucket_mode(value) {
        Some(mode @ (LayerStackBucketMode::KingRank9 | LayerStackBucketMode::ProgressKPAbs)) => {
            Ok(mode)
        }
        Some(LayerStackBucketMode::ProgressKPAbsQ16) => bail!(
            "progresskpabsq16 is not supported by tools native evaluation; use a Q16-capable USI engine with LS_BUCKET_MODE=progresskpabsq16, LS_PROGRESS_BUCKETS and LS_PROGRESS_COEFF (do not substitute progresskpabs)"
        ),
        _ => bail!(
            "unsupported native LayerStacks bucket mode '{value}'; expected progresskpabs or kingrank9"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_routing_rejects_q16_accepted_by_core() {
        for value in ["progresskpabsq16", " PROGRESSKPABSQ16 "] {
            assert_eq!(
                parse_layer_stack_bucket_mode(value),
                Some(LayerStackBucketMode::ProgressKPAbsQ16)
            );
            let error = parse_native_layer_stack_bucket_mode(value).unwrap_err().to_string();
            assert!(error.contains("not supported by tools native evaluation"));
            assert!(error.contains("Q16-capable USI engine"));
        }
    }

    #[test]
    fn native_routing_preserves_supported_modes_and_rejects_typos() {
        assert_eq!(
            parse_native_layer_stack_bucket_mode(" KINGRANK9 ").unwrap(),
            LayerStackBucketMode::KingRank9
        );
        assert_eq!(
            parse_native_layer_stack_bucket_mode("PROGRESSKPABS").unwrap(),
            LayerStackBucketMode::ProgressKPAbs
        );
        assert!(parse_native_layer_stack_bucket_mode("progress8kpabs").is_err());
    }
}
