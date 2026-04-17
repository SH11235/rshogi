//! Phase ゲート（Requirement 15.7）。
//!
//! Phase 1 受入基準が充足するまで Phase 2 以降の機能を本番ビルドに含めないための
//! 仕掛け。運用中のミスで Phase 2+ 機能が TCP バイナリに同梱されないよう、
//! ビルド時に以下の不変条件を保証する:
//!
//! 1. **クレート構成ゲート**: 本クレートは `rshogi-csa-server` を
//!    `default-features = false, features = ["tokio-transport"]` で引き込み、
//!    Cloudflare Workers 向けの `workers` feature を意図的に取り込まない。
//!    → `Cargo.toml` で表現している（静的）。
//! 2. **機能衝突ゲート**: `tokio-transport` と Phase 2 以降の `workers` が
//!    同時に有効化された場合、下記の `compile_error!` でビルドを停止する。
//!    feature unification で上位依存が誤って両方有効化しても検出できる。
//! 3. **Phase ロック定数**: [`PHASE1_LOCK`] を手で書き換えない限り、本バイナリが
//!    自動的に Phase 2 モードへ昇格することはない。Phase 2 移行時は
//!    [`CURRENT_PHASE`] を更新して本ファイルをレビューした上で、
//!    Phase 1 受入シナリオの再走行（`tests/e2e_phase1.rs` 全緑）を確認する運用。
//!
//! これらは `docs/csa-server/design.md` の Phase ゲート方針と一致する。
//! `assert_phase1_only()` は `main` と統合テストから呼ばれ、const 評価で
//! 不整合を検出する（実行時コストはゼロ）。

// (1) 機能衝突ゲート: `rshogi-csa-server` 側の `workers` と `tokio-transport` は
//     Phase 1 → Phase 2 移行期にのみ両立する設計で、本 Phase 1 バイナリでは
//     絶対に workers 経路を含めない。feature unification で workers が混入したら
//     ビルドを止める（defensive gate）。
#[cfg(feature = "workers")]
compile_error!(
    "rshogi-csa-server-tcp: Phase 1 gate violated — `workers` feature cannot be enabled \
     together with the TCP frontend. Bump the phase lock (see phase_gate.rs) before merging."
);

/// 本ビルドが想定している Phase 番号。値は [`PHASE1_LOCK`] と一致する必要がある。
///
/// Phase 2 へ進める際は以下を同時に更新する:
/// 1. [`CURRENT_PHASE`] を次 Phase の番号に書き換える。
/// 2. [`PHASE1_LOCK`] はそのまま残し、受入検証用の定数として保持する。
/// 3. `tests/e2e_phase1.rs` の緑 + 新 Phase の受入テストを追加し合格を確認する。
pub const CURRENT_PHASE: u32 = 1;

/// Phase 1 ロック定数。Phase 1 受入基準の合格時点で `1` に昇格済み、という宣言。
/// Phase 2 のコードが混入する前提下で本値を変えない（[`assert_phase1_only`] の
/// 左辺として使う）。
pub const PHASE1_LOCK: u32 = 1;

/// 本バイナリがコンパイル時に Phase 1 ロックを満たしているかを const 評価で検証する。
///
/// ロック解除手順:
/// 1. Phase 1 受入シナリオ（`cargo test -p rshogi-csa-server-tcp`）が全緑。
/// 2. Phase 2 実装が完成し、独立した Phase 2 ゲートが新設されている。
/// 3. 本関数の呼び出し側を Phase 2 ゲートへ差し替える。
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

    /// Phase 2+ が混入していないかをテストから確認するためのブール。
    /// `cfg(feature = "workers")` が立っていれば上の `compile_error!` で落ちるため、
    /// 到達したら false を返すはずがない（= 常に true）。
    pub const fn phase1_only() -> bool {
        #[cfg(feature = "workers")]
        {
            false
        }
        #[cfg(not(feature = "workers"))]
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

    #[test]
    fn phase_gate_asserts_no_workers_feature() {
        // `workers` feature が立つとそもそも compile_error! で落ちるため、
        // 本テストが走ること自体が Phase 1 ゲートの有効性を示す。
        assert!(PhaseGate::phase1_only());
    }
}
