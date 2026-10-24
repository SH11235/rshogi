//! tools の native 評価経路が係数ロードまで対応する routing mode の境界。
use anyhow::{Result, bail};
use rshogi_core::nnue::{
    LayerStackBucketMode, LayerStackRoutingError, configure_layer_stack_routing,
    parse_layer_stack_bucket_mode, set_layer_stack_progress_kpabs_weights,
    validate_layer_stack_routing_with_coefficients,
};

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

/// native routing の検証済み設定。モデルや FV_SCALE のロードは含まない。
#[derive(Clone, Copy, Debug)]
pub struct NativeRouting {
    mode: LayerStackBucketMode,
    stored: usize,
    buckets: Option<usize>,
}

impl NativeRouting {
    /// 非 LayerStacks は routing オプションがなければ `None` を返す。
    pub fn prepare(
        stored: Option<usize>,
        mode: Option<LayerStackBucketMode>,
        buckets: Option<usize>,
        coefficients_present: bool,
    ) -> Result<Option<Self>> {
        if let Some(mode) = mode {
            parse_native_layer_stack_bucket_mode(mode.as_str())?;
        }
        let Some(stored) = stored else {
            if mode.is_some() || buckets.is_some() || coefficients_present {
                bail!("LayerStacks routing options were provided for a non-LayerStacks network");
            }
            return Ok(None);
        };
        let mode =
            mode.ok_or_else(|| anyhow::anyhow!("LayerStacks requires an explicit bucket mode"))?;
        let plan = Self {
            mode,
            stored,
            buckets,
        };
        plan.validate(coefficients_present)?;
        Ok(Some(plan))
    }

    fn validate(&self, coefficients_present: bool) -> Result<()> {
        validate_layer_stack_routing_with_coefficients(
            self.mode,
            self.stored,
            self.buckets,
            coefficients_present,
        )
        .map_err(|error| match error {
            LayerStackRoutingError::Configuration(message) => anyhow::anyhow!(message),
            LayerStackRoutingError::MissingCoefficients => {
                anyhow::anyhow!("progresskpabs routing requires progress coefficients")
            }
            LayerStackRoutingError::UnusedCoefficients => {
                anyhow::anyhow!("kingrank9 routing does not use progress coefficients")
            }
        })
    }

    /// 検証と係数ロードが成功してから global の係数・routing を更新する。
    /// loader は global を変更せず、呼出元で既読の bytes や係数も利用できる。
    pub fn apply(self, loader: impl FnOnce() -> Result<Option<Box<[f32]>>>) -> Result<()> {
        let weights = loader()?;
        self.validate(weights.is_some())?;
        if let Some(weights) = weights {
            set_layer_stack_progress_kpabs_weights(weights).map_err(anyhow::Error::msg)?;
        }
        configure_layer_stack_routing(self.mode, self.stored, self.buckets)
            .map_err(anyhow::Error::msg)
    }

    /// 検証済みの mode。
    pub fn mode(self) -> LayerStackBucketMode {
        self.mode
    }

    /// progress routing の bucket 数。KingRank9 は `None`。
    pub fn progress_buckets(self) -> Option<usize> {
        if self.mode == LayerStackBucketMode::ProgressKPAbs {
            self.buckets
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_configuration_matrix() {
        use LayerStackBucketMode::{KingRank9, ProgressKPAbs, ProgressKPAbsQ16};
        for mode in [ProgressKPAbs, ProgressKPAbsQ16] {
            for stored in [1, 8, 9] {
                for buckets in [None, Some(0), Some(1), Some(8), Some(10)] {
                    for coefficients in [false, true] {
                        let valid = buckets.is_some_and(|n| n > 0 && n <= stored)
                            && (buckets == Some(1) || coefficients);
                        assert_eq!(
                            validate_layer_stack_routing_with_coefficients(
                                mode,
                                stored,
                                buckets,
                                coefficients
                            )
                            .is_ok(),
                            valid,
                            "mode={mode:?}, stored={stored}, buckets={buckets:?}, coefficients={coefficients}"
                        );
                        assert_eq!(
                            NativeRouting::prepare(Some(stored), Some(mode), buckets, coefficients)
                                .is_ok(),
                            valid && mode != ProgressKPAbsQ16
                        );
                    }
                }
            }
        }
        for stored in [1, 8, 9] {
            for buckets in [None, Some(1), Some(9)] {
                for coefficients in [false, true] {
                    assert_eq!(
                        NativeRouting::prepare(
                            Some(stored),
                            Some(KingRank9),
                            buckets,
                            coefficients
                        )
                        .is_ok(),
                        stored == 9 && buckets.is_none() && !coefficients
                    );
                }
            }
        }
        assert!(NativeRouting::prepare(None, None, None, false).unwrap().is_none());
        assert!(NativeRouting::prepare(None, Some(ProgressKPAbs), None, false).is_err());
        assert!(NativeRouting::prepare(None, None, Some(1), false).is_err());
        assert!(NativeRouting::prepare(None, None, None, true).is_err());
        assert!(NativeRouting::prepare(Some(1), None, Some(1), false).is_err());
    }

    #[test]
    fn failed_native_setup_preserves_global_routing_and_weights() {
        const CHILD: &str = "RSHOGI_ROUTING_FAILURE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "nnue_routing::tests::failed_native_setup_preserves_global_routing_and_weights",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            return;
        }
        use rshogi_core::nnue::{
            SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, get_layer_stack_bucket_mode,
            get_layer_stack_progress_buckets, get_layer_stack_progress_kpabs_weights,
        };
        let baseline = vec![0.25; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS].into_boxed_slice();
        set_layer_stack_progress_kpabs_weights(baseline.clone()).unwrap();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbs, 8, Some(8)).unwrap();
        let unchanged = || {
            assert_eq!(get_layer_stack_bucket_mode(), LayerStackBucketMode::ProgressKPAbs);
            assert_eq!(get_layer_stack_progress_buckets(), Some(8));
            assert_eq!(&*get_layer_stack_progress_kpabs_weights(), &*baseline);
        };
        assert!(
            NativeRouting::prepare(Some(9), Some(LayerStackBucketMode::KingRank9), None, true)
                .is_err()
        );
        unchanged();
        let plan = NativeRouting::prepare(
            Some(1),
            Some(LayerStackBucketMode::ProgressKPAbs),
            Some(1),
            true,
        )
        .unwrap()
        .unwrap();
        assert!(plan.apply(|| bail!("coefficient read failed")).is_err());
        unchanged();
        assert!(plan.apply(|| Ok(Some(vec![1.0].into_boxed_slice()))).is_err());
        unchanged();
        let plan = NativeRouting::prepare(
            Some(8),
            Some(LayerStackBucketMode::ProgressKPAbs),
            Some(8),
            true,
        )
        .unwrap()
        .unwrap();
        assert!(plan.apply(|| Ok(None)).is_err());
        unchanged();
    }

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
