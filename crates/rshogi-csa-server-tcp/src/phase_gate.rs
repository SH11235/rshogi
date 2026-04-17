//! Phase ゲート（Requirement 15.7）の基礎定数と静的アサーション。
//!
//! 本モジュールは Phase 1 のコア定数（[`CURRENT_PHASE`] / [`PHASE1_LOCK`]）と、
//! const 評価時にロック状態を検証する [`assert_phase1_only`] を提供する。
//!
//! Phase 2 移行時は本ファイルをレビューした上で定数と呼び出し側を差し替え、
//! Phase 1 受入シナリオの再走行（`tests/e2e_phase1.rs` 全緑）を確認する運用とする。

/// 本ビルドが想定している Phase 番号。値は [`PHASE1_LOCK`] と一致する必要がある。
pub const CURRENT_PHASE: u32 = 1;

/// Phase 1 ロック定数。Phase 1 受入基準の合格時点で `1` に昇格済み、という宣言。
pub const PHASE1_LOCK: u32 = 1;

/// 本バイナリがコンパイル時に Phase 1 ロックを満たしているかを const 評価で検証する。
///
/// `main.rs` と Phase 1 の統合テストから呼び、const 評価の副作用として静的検証する。
pub const fn assert_phase1_only() {
    // `CURRENT_PHASE != PHASE1_LOCK` のとき const 評価で `panic!` を発動させ、
    // コンパイルエラーに落とす（ランタイムコストはゼロ）。
    const _CHECK: () = {
        if CURRENT_PHASE != PHASE1_LOCK {
            panic!(
                "rshogi-csa-server-tcp: Phase lock mismatch. \
                 CURRENT_PHASE must equal PHASE1_LOCK for Phase 1 builds."
            );
        }
    };
    // const 評価を呼び出し側に結び付けて未使用除去を防ぐ。
    _CHECK
}

/// 静的ゲート情報を文字列で返す（ヘルスチェック応答・起動ログ用）。
pub struct PhaseGate;

impl PhaseGate {
    /// 起動ログなどに表示するラベル。`phase=1 locked=1 tcp-only` の形式で、
    /// 実運用で「このバイナリが本当に Phase 1 か」を目視確認できるようにする。
    pub const fn label() -> &'static str {
        "phase=1 locked=1 tcp-only"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_constants_are_in_sync() {
        assert_eq!(CURRENT_PHASE, PHASE1_LOCK);
        assert_eq!(CURRENT_PHASE, 1);
    }

    #[test]
    fn assert_phase1_only_is_const_safe() {
        // const fn として呼び出せる = 定数評価に通っている = ロック成立。
        assert_phase1_only();
    }

    #[test]
    fn phase_gate_label_is_stable() {
        assert_eq!(PhaseGate::label(), "phase=1 locked=1 tcp-only");
    }
}
