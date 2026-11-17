//! forward の固定区間を累積する、任意の wall clock 診断。

/// 一回の forward 内の計測区間。転送・同期は発生した区間の壁時計時間に含む。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum InferenceStage {
    /// 入力を借用する TensorRef の構築。
    TensorRefs,
    /// 出力 MemoryInfo と binding の構築。
    BindingSetup,
    /// 全入力の bind。runtime による転送を含み得る。
    InputBind,
    /// 全出力の bind。
    OutputBind,
    /// run_binding と出力 value の構築。CPU 出力への転送と device 同期を含む。
    RunBinding,
    /// shape・有限性の検査と呼出し側 buffer への copy。
    OutputValidationCopy,
    /// 出力・binding・MemoryInfo・入力 TensorRef の解放。
    Cleanup,
}

impl InferenceStage {
    /// 固定の区間数。
    pub const COUNT: usize = 7;
    /// forward の実行順。
    pub const ALL: [Self; Self::COUNT] = [
        Self::TensorRefs,
        Self::BindingSetup,
        Self::InputBind,
        Self::OutputBind,
        Self::RunBinding,
        Self::OutputValidationCopy,
        Self::Cleanup,
    ];

    /// JSON 等へ出す固定名。
    pub fn name(self) -> &'static str {
        match self {
            Self::TensorRefs => "tensor_refs",
            Self::BindingSetup => "binding_setup",
            Self::InputBind => "input_bind",
            Self::OutputBind => "output_bind",
            Self::RunBinding => "run_binding",
            Self::OutputValidationCopy => "output_validation_copy",
            Self::Cleanup => "cleanup",
        }
    }
}

/// 一区間の累積値。各 counter は u64::MAX で飽和し、wrap しない。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StageTiming {
    /// 区間に入った forward の数。入力 bind の個々の API 呼出し数ではない。
    pub calls: u64,
    /// この区間で Result::Err となった数。
    pub errors: u64,
    /// 累積 wall clock nanoseconds。
    pub nanoseconds: u64,
}

/// evaluator 構築以降の forward 診断。encode・cache 照合・forward 前の引数検査は含まない。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InferenceTimings {
    /// forward に入った数。失敗を含む。
    pub forward_calls: u64,
    /// forward が失敗した数。
    pub forward_failures: u64,
    /// forward に渡した局面数。失敗を含む。
    pub positions: u64,
    /// InferenceStage の順の固定配列。
    pub stages: [StageTiming; InferenceStage::COUNT],
}

impl InferenceTimings {
    /// 同じ evaluator の以前の snapshot との差分。counter が逆行した場合は None。
    pub fn checked_since(self, before: Self) -> Option<Self> {
        let mut result = Self {
            forward_calls: self.forward_calls.checked_sub(before.forward_calls)?,
            forward_failures: self.forward_failures.checked_sub(before.forward_failures)?,
            positions: self.positions.checked_sub(before.positions)?,
            stages: [StageTiming::default(); InferenceStage::COUNT],
        };
        for ((output, after), before) in
            result.stages.iter_mut().zip(self.stages).zip(before.stages)
        {
            *output = StageTiming {
                calls: after.calls.checked_sub(before.calls)?,
                errors: after.errors.checked_sub(before.errors)?,
                nanoseconds: after.nanoseconds.checked_sub(before.nanoseconds)?,
            };
        }
        Some(result)
    }
}

#[cfg(feature = "onnx")]
pub(crate) struct ForwardTimer<'a> {
    target: &'a mut InferenceTimings,
    local: InferenceTimings,
    stage: InferenceStage,
    started: std::time::Instant,
    success: bool,
}

#[cfg(feature = "onnx")]
impl<'a> ForwardTimer<'a> {
    pub(crate) fn new(target: &'a mut InferenceTimings, positions: usize) -> Self {
        Self {
            target,
            local: InferenceTimings {
                forward_calls: 1,
                positions: u64::try_from(positions).unwrap_or(u64::MAX),
                ..InferenceTimings::default()
            },
            stage: InferenceStage::TensorRefs,
            started: std::time::Instant::now(),
            success: false,
        }
    }

    fn record(&mut self, failed: bool) {
        let timing = &mut self.local.stages[self.stage as usize];
        timing.calls = timing.calls.saturating_add(1);
        timing.errors = timing.errors.saturating_add(u64::from(failed));
        timing.nanoseconds = timing
            .nanoseconds
            .saturating_add(u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }

    pub(crate) fn advance(&mut self, stage: InferenceStage) {
        self.record(false);
        self.stage = stage;
        self.started = std::time::Instant::now();
    }

    pub(crate) fn fail(&mut self) {
        self.record(true);
        self.stage = InferenceStage::Cleanup;
        self.started = std::time::Instant::now();
    }

    pub(crate) fn finish(&mut self) {
        self.advance(InferenceStage::Cleanup);
        self.success = true;
    }
}

#[cfg(feature = "onnx")]
impl Drop for ForwardTimer<'_> {
    fn drop(&mut self) {
        self.record(!self.success && self.stage != InferenceStage::Cleanup);
        self.local.forward_failures = u64::from(!self.success);
        self.target.forward_calls =
            self.target.forward_calls.saturating_add(self.local.forward_calls);
        self.target.forward_failures =
            self.target.forward_failures.saturating_add(self.local.forward_failures);
        self.target.positions = self.target.positions.saturating_add(self.local.positions);
        for (target, local) in self.target.stages.iter_mut().zip(self.local.stages) {
            target.calls = target.calls.saturating_add(local.calls);
            target.errors = target.errors.saturating_add(local.errors);
            target.nanoseconds = target.nanoseconds.saturating_add(local.nanoseconds);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_are_fixed_copy_values_and_refuse_counter_regression() {
        let mut before = InferenceTimings {
            forward_calls: 2,
            positions: 8,
            ..InferenceTimings::default()
        };
        before.stages[InferenceStage::InputBind as usize] = StageTiming {
            calls: 2,
            errors: 1,
            nanoseconds: 10,
        };
        let mut after = before;
        after.forward_calls += 3;
        after.positions += 12;
        after.stages[InferenceStage::InputBind as usize].nanoseconds += 20;
        let delta = after.checked_since(before).unwrap();
        assert_eq!(delta.forward_calls, 3);
        assert_eq!(delta.positions, 12);
        assert_eq!(delta.stages[InferenceStage::InputBind as usize].nanoseconds, 20);
        assert_eq!(delta.stages[InferenceStage::InputBind as usize].errors, 0);
        assert!(before.checked_since(after).is_none());
        let mut invalid = after;
        invalid.stages[InferenceStage::InputBind as usize].errors = 0;
        assert!(invalid.checked_since(before).is_none());
    }

    #[cfg(feature = "onnx")]
    #[test]
    fn failure_records_its_stage_and_cleanup_without_wrapping_counters() {
        let mut totals = InferenceTimings::default();
        {
            let mut timer = ForwardTimer::new(&mut totals, 4);
            timer.advance(InferenceStage::BindingSetup);
            timer.fail();
        }
        assert_eq!((totals.forward_calls, totals.forward_failures, totals.positions), (1, 1, 4));
        assert_eq!(totals.stages[InferenceStage::TensorRefs as usize].calls, 1);
        assert_eq!(totals.stages[InferenceStage::BindingSetup as usize].errors, 1);
        assert_eq!(totals.stages[InferenceStage::Cleanup as usize].calls, 1);
        assert_eq!(totals.stages[InferenceStage::RunBinding as usize].calls, 0);
        totals.forward_calls = u64::MAX;
        totals.positions = u64::MAX;
        {
            let mut timer = ForwardTimer::new(&mut totals, 1);
            timer.finish();
        }
        assert_eq!(totals.forward_calls, u64::MAX);
        assert_eq!(totals.positions, u64::MAX);
    }
}
