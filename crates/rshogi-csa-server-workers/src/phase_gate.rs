//! Phase 2 ゲート。
//!
//! Phase 1 受入基準を満たした上で Phase 2 (Cloudflare Workers 対応) に進んだ
//! ことを宣言する。Phase 3 以降の機能が本 crate の本番ビルドに混入しないよう、
//! TCP 側の `phase_gate.rs` と同じ責務のガードを用意する。
//!
//! # 前提
//!
//! 1. **依存ゲート**: 本 crate は `rshogi-csa-server` を
//!    `default-features = false, features = ["workers"]` で引き込み、
//!    TCP 向けの `tokio-transport` は取り込まない（`Cargo.toml` で静的に表現）。
//! 2. **機能衝突ゲート**: `tokio-transport` feature が本 crate 経由で有効化
//!    された場合、下記 `compile_error!` でビルドを止める（feature unification で
//!    上位依存が誤って両方を有効化しても検出できる）。
//! 3. **Phase ロック定数**: [`PHASE2_LOCK`] は手で書き換えない限り
//!    Phase 3 以降の機能を勝手に取り込まない。Phase 3 移行時は
//!    [`CURRENT_PHASE`] を更新し、Phase 2 受入シナリオの再走行を確認する運用。

// `tokio-transport` が本 crate のビルドで有効化されるのは想定外。Workers 用の
// wasm32 ランタイムは tokio マルチスレッドを扱わないため、defensive に落とす。
#[cfg(feature = "tokio-transport")]
compile_error!(
    "rshogi-csa-server-workers: Phase 2 gate violated — `tokio-transport` feature cannot be \
     enabled together with the Cloudflare Workers frontend. Check feature unification."
);

/// 本 crate の現在 Phase。[`PHASE2_LOCK`] と一致する必要がある。
///
/// Phase 3 へ進める際は以下を同時に更新する:
/// 1. [`CURRENT_PHASE`] を次 Phase の番号に書き換える。
/// 2. [`PHASE2_LOCK`] はそのまま残し、受入検証用の定数として保持する。
/// 3. Phase 2 受入シナリオ（tasks.md §9.7）が全緑であることを確認する。
pub const CURRENT_PHASE: u32 = 2;

/// Phase 2 ロック定数。Phase 2 受入基準の合格時点で `2` に昇格する宣言。
/// Phase 3 以降のコードが混入しても本値を変えない（[`assert_phase2_only`] の
/// 左辺として使う）。
pub const PHASE2_LOCK: u32 = 2;

/// 本 crate がコンパイル時に Phase 2 ロックを満たしているかを const 評価で検証する。
///
/// ロック解除手順:
/// 1. Phase 2 受入シナリオ（`cargo test -p rshogi-csa-server-workers` 相当）が全緑。
/// 2. Phase 3 実装が完成し、独立した Phase 3 ゲートが新設されている。
/// 3. 本関数の呼び出し側を Phase 3 ゲートへ差し替える。
pub const fn assert_phase2_only() {
    const _CHECK: () = {
        if CURRENT_PHASE != PHASE2_LOCK {
            panic!(
                "rshogi-csa-server-workers: Phase lock mismatch. \
                 CURRENT_PHASE must equal PHASE2_LOCK for Phase 2 builds."
            );
        }
    };
    _CHECK
}

/// 静的ゲート情報を文字列で返す（ヘルスチェック応答・起動ログ用）。
pub struct PhaseGate;

impl PhaseGate {
    /// 起動ログなどに表示するラベル。`phase=2 locked=2 workers` の形式で、
    /// 実運用で「このバイナリが本当に Phase 2 か」を目視確認できるようにする。
    pub const fn label() -> &'static str {
        "phase=2 locked=2 workers"
    }

    /// Phase 3+ が混入していないかをテストから確認するためのブール。
    /// `cfg(feature = "tokio-transport")` が立っていれば上の `compile_error!` で
    /// 落ちるため、到達したら false を返すはずがない（= 常に true）。
    pub const fn phase2_only() -> bool {
        #[cfg(feature = "tokio-transport")]
        {
            false
        }
        #[cfg(not(feature = "tokio-transport"))]
        {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_constants_are_in_sync() {
        assert_eq!(CURRENT_PHASE, PHASE2_LOCK);
        assert_eq!(CURRENT_PHASE, 2);
    }

    #[test]
    fn assert_phase2_only_is_const_safe() {
        assert_phase2_only();
    }

    #[test]
    fn phase_gate_label_is_stable() {
        assert_eq!(PhaseGate::label(), "phase=2 locked=2 workers");
    }

    #[test]
    fn phase_gate_asserts_no_tokio_transport_feature() {
        // `tokio-transport` feature が立つとそもそも compile_error! で落ちるため、
        // 本テストが走ること自体が Phase 2 ゲートの有効性を示す。
        assert!(PhaseGate::phase2_only());
    }
}
