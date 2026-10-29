//! tools の native 評価経路が係数ロードまで対応する routing mode の境界。
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rshogi_core::nnue::{
    LayerStackBucketMode, LayerStackRoutingError, compute_layer_stack_kingrank9_bucket_index,
    compute_progresskpabs_q16_sum, compute_progresskpabs_sum, configure_layer_stack_routing,
    get_layer_stack_bucket_mode, get_layer_stack_progress_kpabs_q16_weights,
    get_layer_stack_progress_kpabs_weights, load_progress_coeff_kpabs_from_bytes,
    load_progress_coeff_kpabs_q16_from_bytes, parse_layer_stack_bucket_mode,
    progress_q16_sum_to_bucket, progress_sum_to_bucket,
    reset_layer_stack_progress_kpabs_q16_weights, set_layer_stack_progress_kpabs_q16_weights,
    set_layer_stack_progress_kpabs_weights, validate_layer_stack_routing_with_coefficients,
};
use rshogi_core::position::Position;

/// tools の native 経路で実装済みの mode だけ受理する。
/// core の parser に mode が増えても係数未設定のまま評価へ進めない。
pub fn parse_native_layer_stack_bucket_mode(value: &str) -> Result<LayerStackBucketMode> {
    match parse_layer_stack_bucket_mode(value) {
        Some(
            mode @ (LayerStackBucketMode::KingRank9
            | LayerStackBucketMode::ProgressKPAbs
            | LayerStackBucketMode::ProgressKPAbsQ16),
        ) => Ok(mode),
        _ => bail!(
            "unsupported native LayerStacks bucket mode '{value}'; expected progresskpabs, progresskpabsq16 or kingrank9"
        ),
    }
}

/// progress.bin から読んだ routing 係数。
///
/// Q16 は YaneuraOu と同じく f64 の元値を `round(w * 65536)` するので、f32 へ丸めた後の値からは
/// 作れない。f64 の progress.bin から読んだときだけ Q16 係数を持つ。
pub struct ProgressCoefficients {
    kpabs: Box<[f32]>,
    q16: Option<Box<[i32]>>,
}

impl ProgressCoefficients {
    /// f64 LE の progress.bin の bytes から f32 と Q16 の両係数を作る。
    pub fn from_f64_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            kpabs: load_progress_coeff_kpabs_from_bytes(bytes).map_err(anyhow::Error::msg)?,
            q16: Some(load_progress_coeff_kpabs_q16_from_bytes(bytes).map_err(anyhow::Error::msg)?),
        })
    }

    /// f64 LE の progress.bin を読む。
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read progress coefficients {}", path.display()))?;
        Self::from_f64_bytes(&bytes)
            .with_context(|| format!("invalid progress coefficients {}", path.display()))
    }

    /// f32 係数だけを持つ。`progresskpabsq16` には使えない。
    pub fn from_kpabs(kpabs: Box<[f32]>) -> Self {
        Self { kpabs, q16: None }
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
                anyhow::anyhow!("{} routing requires progress coefficients", self.mode.as_str())
            }
            LayerStackRoutingError::UnusedCoefficients => {
                anyhow::anyhow!("kingrank9 routing does not use progress coefficients")
            }
        })
    }

    /// 検証と係数ロードが成功してから global の係数・routing を更新する。
    /// loader は global を変更せず、呼出元で既読の bytes や係数も利用できる。
    pub fn apply(
        self,
        loader: impl FnOnce() -> Result<Option<ProgressCoefficients>>,
    ) -> Result<()> {
        let coefficients = loader()?;
        self.validate(coefficients.is_some())?;
        if self.mode == LayerStackBucketMode::ProgressKPAbsQ16
            && coefficients.as_ref().is_some_and(|c| c.q16.is_none())
        {
            bail!("progresskpabsq16 routing requires f64 progress coefficients");
        }
        if let Some(ProgressCoefficients { kpabs, q16 }) = coefficients {
            set_layer_stack_progress_kpabs_weights(kpabs).map_err(anyhow::Error::msg)?;
            match q16 {
                Some(q16) => {
                    set_layer_stack_progress_kpabs_q16_weights(q16).map_err(anyhow::Error::msg)?;
                }
                None => reset_layer_stack_progress_kpabs_q16_weights(),
            }
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
        match self.mode {
            LayerStackBucketMode::ProgressKPAbs | LayerStackBucketMode::ProgressKPAbsQ16 => {
                self.buckets
            }
            _ => None,
        }
    }
}

/// 探索 stack を使わずに bucket を数えるツール向けの、係数を取得済みの bucket 計算。
///
/// 係数は生成時に一度だけ取得し、評価ループ中は global を読まない。
pub enum LayerStackBucketer {
    /// 進行度方式の routing bucket 数 1。係数を参照せず常に bucket 0。
    Single,
    KingRank9 {
        stored: usize,
    },
    ProgressKpAbs {
        weights: Arc<[f32]>,
        buckets: usize,
    },
    ProgressKpAbsQ16 {
        weights: Arc<[i32]>,
        buckets: usize,
    },
}

impl LayerStackBucketer {
    /// 適用済みの routing と係数から作る。`routing_buckets` は progress 系の routing bucket 数。
    pub fn from_configured(stored: usize, routing_buckets: usize) -> Result<Self> {
        Ok(match get_layer_stack_bucket_mode() {
            LayerStackBucketMode::KingRank9 => Self::KingRank9 { stored },
            // routing 数 1 では係数ファイルを省略できるので、未設定の係数を要求しない。
            LayerStackBucketMode::ProgressKPAbs | LayerStackBucketMode::ProgressKPAbsQ16
                if routing_buckets == 1 =>
            {
                Self::Single
            }
            LayerStackBucketMode::ProgressKPAbs => Self::ProgressKpAbs {
                weights: Arc::from(&*get_layer_stack_progress_kpabs_weights()),
                buckets: routing_buckets,
            },
            LayerStackBucketMode::ProgressKPAbsQ16 => Self::ProgressKpAbsQ16 {
                weights: get_layer_stack_progress_kpabs_q16_weights()
                    .context("progresskpabsq16 coefficients are not configured")?,
                buckets: routing_buckets,
            },
            mode => bail!("unsupported LayerStacks bucket mode {}", mode.as_str()),
        })
    }

    /// 局面の bucket index。
    pub fn bucket(&self, pos: &Position) -> usize {
        match self {
            Self::Single => 0,
            Self::KingRank9 { stored } => {
                compute_layer_stack_kingrank9_bucket_index(pos, pos.side_to_move(), *stored)
            }
            Self::ProgressKpAbs { weights, buckets } => {
                progress_sum_to_bucket(compute_progresskpabs_sum(pos, weights), *buckets)
            }
            Self::ProgressKpAbsQ16 { weights, buckets } => {
                progress_q16_sum_to_bucket(compute_progresskpabs_q16_sum(pos, weights), *buckets)
            }
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
                            valid
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
        assert!(
            plan.apply(|| Ok(Some(ProgressCoefficients::from_kpabs(vec![1.0].into_boxed_slice()))))
                .is_err()
        );
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
        let plan = NativeRouting::prepare(
            Some(8),
            Some(LayerStackBucketMode::ProgressKPAbsQ16),
            Some(8),
            true,
        )
        .unwrap()
        .unwrap();
        let f32_only = || {
            Ok(Some(ProgressCoefficients::from_kpabs(
                vec![1.0; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS].into_boxed_slice(),
            )))
        };
        let error = plan.apply(f32_only).unwrap_err().to_string();
        assert!(error.contains("requires f64 progress coefficients"), "{error}");
        unchanged();
    }

    /// 同じ f64 係数から、f32 と Q16 の bucket が core の全走査と一致することを確かめる。
    #[test]
    fn applied_coefficients_route_like_core_full_scan() {
        const CHILD: &str = "RSHOGI_ROUTING_APPLY_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "nnue_routing::tests::applied_coefficients_route_like_core_full_scan",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            return;
        }
        use rshogi_core::movegen::{MoveList, generate_legal};
        use rshogi_core::nnue::{
            SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, compute_layer_stack_progresskpabs_bucket_index,
        };
        use rshogi_core::position::SFEN_HIRATE;

        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let raw: Vec<f64> = (0..SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS)
            .map(|_| (next() % 20_001) as f64 / 100_000.0 - 0.1)
            .collect();
        let bytes: Vec<u8> = raw.iter().flat_map(|w| w.to_le_bytes()).collect();
        let q16 = load_progress_coeff_kpabs_q16_from_bytes(&bytes).unwrap();
        let kpabs = load_progress_coeff_kpabs_from_bytes(&bytes).unwrap();

        let mut positions = Vec::new();
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        for _ in 0..200 {
            let mut moves = MoveList::new();
            generate_legal(&pos, &mut moves);
            let moves: Vec<_> = moves.iter().copied().collect();
            if moves.is_empty() {
                break;
            }
            let mv = moves[(next() % moves.len() as u64) as usize];
            pos.do_move(mv, pos.gives_check(mv));
            positions.push(pos.clone());
        }

        for mode in [
            LayerStackBucketMode::ProgressKPAbs,
            LayerStackBucketMode::ProgressKPAbsQ16,
        ] {
            NativeRouting::prepare(Some(8), Some(mode), Some(8), true)
                .unwrap()
                .unwrap()
                .apply(|| ProgressCoefficients::from_f64_bytes(&bytes).map(Some))
                .unwrap();
            let bucketer = LayerStackBucketer::from_configured(8, 8).unwrap();
            for pos in &positions {
                let expected = match mode {
                    LayerStackBucketMode::ProgressKPAbsQ16 => {
                        progress_q16_sum_to_bucket(compute_progresskpabs_q16_sum(pos, &q16), 8)
                    }
                    _ => compute_layer_stack_progresskpabs_bucket_index(
                        pos,
                        pos.side_to_move(),
                        &kpabs,
                        8,
                    ),
                };
                assert_eq!(bucketer.bucket(pos), expected, "{mode:?} {}", pos.to_sfen());
            }
        }
    }

    #[test]
    fn native_routing_accepts_supported_modes_and_rejects_typos() {
        assert_eq!(
            parse_native_layer_stack_bucket_mode(" KINGRANK9 ").unwrap(),
            LayerStackBucketMode::KingRank9
        );
        assert_eq!(
            parse_native_layer_stack_bucket_mode("PROGRESSKPABS").unwrap(),
            LayerStackBucketMode::ProgressKPAbs
        );
        for value in ["progresskpabsq16", " PROGRESSKPABSQ16 "] {
            assert_eq!(
                parse_native_layer_stack_bucket_mode(value).unwrap(),
                LayerStackBucketMode::ProgressKPAbsQ16
            );
        }
        assert!(parse_native_layer_stack_bucket_mode("progress8kpabs").is_err());
    }
}
