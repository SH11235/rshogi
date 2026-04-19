//! 同一 IP からの LOGIN 試行レート制限。
//!
//! - 直近 1 分間に 10 回（既定値）を超える LOGIN 試行があれば、以降 5 分間は拒否する。
//! - 失敗・成功を問わずカウントする（辞書攻撃の検出が目的）。
//! - Phase 1 は 1 プロセス内の in-memory map で十分。複数プロセス運用は Phase 5 以降のスコープ。
//!
//! **副作用（設計上の許容事項）**: 集計粒度が IP 単位のため、NAT / プロキシ / 学校・
//! 職場の共有回線配下の無関係なユーザー群が、同一出口 IP の 1 ユーザーの連続失敗に
//! 道連れで `retry_after_sec` の間ロックアウトされる。Phase 1 の運用規模と辞書攻撃
//! 検出優先の方針から本挙動は許容するが、本番運用でユーザー粒度の除外が必要になった
//! 際は `(IpKey, PlayerName)` 複合キーか、認証成功時に一時的な除外リストへ移す形へ
//! 拡張する（Phase 5 以降の運用強化で再評価する）。
//!
//! カウンタはログインレコード `Vec<Instant>` をリング状に保持し、`prune` で古い試行を
//! 捨てる。`record_at` 呼び出しごとに prune + push が走るため要素数は常に `limit + 1`
//! を超えず、走査コストは O(limit) で Phase 1 の規模（10 回/分）には十分に軽い。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rshogi_csa_server::port::RateDecision;
use rshogi_csa_server::types::IpKey;
use tokio::sync::Mutex;

/// 同一 IP からの LOGIN 試行を束ねるレート制限器。
///
/// `Arc<Mutex<...>>` の内部状態を `Clone` で共有できる。1 プロセス内で 1 インスタンス作り、
/// accept ループと LOGIN 処理から共有する設計。
#[derive(Clone)]
pub struct IpLoginRateLimiter {
    inner: Arc<Mutex<Inner>>,
    /// カウント観測窓（既定 60 秒）。この期間内に `limit` を超えたら拒否。
    window: Duration,
    /// 拒否期間（既定 300 秒）。超過以降、このペナルティ期間だけ Deny を返す。
    penalty: Duration,
    /// 窓内の許容回数（既定 10）。
    limit: usize,
}

struct Inner {
    attempts: HashMap<IpKey, AttemptState>,
}

struct AttemptState {
    /// 過去の試行時刻（昇順）。`window` を超えた古いものは `prune` で捨てる。
    timestamps: Vec<Instant>,
    /// 拒否期間中なら、いつまで拒否するか。
    blocked_until: Option<Instant>,
}

impl IpLoginRateLimiter {
    /// 既定パラメタ（60 秒窓で 10 回超過 → 300 秒拒否）で作る。
    pub fn default_limits() -> Self {
        Self::with_limits(10, Duration::from_secs(60), Duration::from_secs(300))
    }

    /// カスタムパラメタで作る（テスト・運用調整用）。
    ///
    /// - `limit`: 窓内の許容回数。
    /// - `window`: 観測窓。
    /// - `penalty`: 超過時の拒否期間。
    pub fn with_limits(limit: usize, window: Duration, penalty: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                attempts: HashMap::new(),
            })),
            window,
            penalty,
            limit,
        }
    }

    /// LOGIN 試行を 1 回カウントし、判定を返す。
    ///
    /// 呼び出し側は [`RateDecision::Deny`] の場合 LOGIN 応答として拒否を送ってから
    /// 接続を閉じること（Phase 1 は rate deny でも CSA コマンドエラーではなくソケット
    /// 切断で良い。CSA 仕様 1.2.1 には rate limit の通知コードがないため）。
    pub async fn record(&self, ip: &IpKey) -> RateDecision {
        self.record_at(ip, Instant::now()).await
    }

    /// 任意の基準時刻で判定するテスト用の内部エンドポイント。
    async fn record_at(&self, ip: &IpKey, now: Instant) -> RateDecision {
        let mut guard = self.inner.lock().await;
        let state = guard.attempts.entry(ip.clone()).or_insert_with(|| AttemptState {
            timestamps: Vec::new(),
            blocked_until: None,
        });

        // 1. ペナルティ期間中ならそのまま Deny。失効済みならクリア。
        if let Some(until) = state.blocked_until {
            if now < until {
                return RateDecision::Deny {
                    retry_after_sec: (until - now).as_secs().max(1),
                };
            }
            state.blocked_until = None;
            state.timestamps.clear();
        }

        // 2. 窓外の古い試行を捨てる（末尾が新しい、先頭が古い並び）。
        let threshold = now.checked_sub(self.window).unwrap_or(now);
        state.timestamps.retain(|t| *t >= threshold);

        // 3. 今回の試行を追加。
        state.timestamps.push(now);

        // 4. 窓内の件数が limit を超えていればペナルティ期間を開始。
        if state.timestamps.len() > self.limit {
            state.blocked_until = Some(now + self.penalty);
            return RateDecision::Deny {
                retry_after_sec: self.penalty.as_secs().max(1),
            };
        }
        RateDecision::Allow
    }

    /// テスト・運用での明示的なクリーンアップ（全 IP 状態を破棄）。
    pub async fn clear_all(&self) {
        let mut guard = self.inner.lock().await;
        guard.attempts.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpKey {
        IpKey::new(s)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn allows_under_limit() {
        let rl =
            IpLoginRateLimiter::with_limits(3, Duration::from_secs(60), Duration::from_secs(300));
        let a = ip("10.0.0.1");
        let now = Instant::now();
        for _ in 0..3 {
            assert_eq!(rl.record_at(&a, now).await, RateDecision::Allow);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn denies_after_limit_exceeded() {
        let rl =
            IpLoginRateLimiter::with_limits(3, Duration::from_secs(60), Duration::from_secs(300));
        let a = ip("10.0.0.2");
        let now = Instant::now();
        for _ in 0..3 {
            rl.record_at(&a, now).await;
        }
        // 4 回目で Deny。
        match rl.record_at(&a, now).await {
            RateDecision::Deny { retry_after_sec } => {
                assert!((299..=300).contains(&retry_after_sec), "{retry_after_sec}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn penalty_expires_after_period() {
        let rl =
            IpLoginRateLimiter::with_limits(2, Duration::from_secs(60), Duration::from_secs(300));
        let a = ip("10.0.0.3");
        let t0 = Instant::now();
        for _ in 0..2 {
            rl.record_at(&a, t0).await;
        }
        // 超過 → Deny
        assert!(matches!(rl.record_at(&a, t0).await, RateDecision::Deny { .. }));
        // ペナルティ開始時刻 + 301 秒 > 開始時刻 + 300 秒 なので Allow に戻る。
        let after_penalty = t0 + Duration::from_secs(301);
        assert_eq!(rl.record_at(&a, after_penalty).await, RateDecision::Allow);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn independent_per_ip() {
        let rl =
            IpLoginRateLimiter::with_limits(1, Duration::from_secs(60), Duration::from_secs(300));
        let now = Instant::now();
        assert_eq!(rl.record_at(&ip("1.1.1.1"), now).await, RateDecision::Allow);
        assert_eq!(rl.record_at(&ip("2.2.2.2"), now).await, RateDecision::Allow);
        // 同一 IP の 2 回目だけが Deny。
        assert!(matches!(rl.record_at(&ip("1.1.1.1"), now).await, RateDecision::Deny { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn old_attempts_are_pruned_outside_window() {
        let rl =
            IpLoginRateLimiter::with_limits(3, Duration::from_secs(60), Duration::from_secs(300));
        let a = ip("10.0.0.4");
        let t0 = Instant::now();
        // 窓外（70 秒前）に 3 回試行 → 現在からは見えない。
        for _ in 0..3 {
            rl.record_at(&a, t0).await;
        }
        // 71 秒後に試行 → Allow（古い試行は prune される）。
        let t1 = t0 + Duration::from_secs(71);
        assert_eq!(rl.record_at(&a, t1).await, RateDecision::Allow);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn clear_all_removes_state() {
        let rl =
            IpLoginRateLimiter::with_limits(1, Duration::from_secs(60), Duration::from_secs(300));
        let a = ip("9.9.9.9");
        let t0 = Instant::now();
        rl.record_at(&a, t0).await;
        assert!(matches!(rl.record_at(&a, t0).await, RateDecision::Deny { .. }));
        rl.clear_all().await;
        assert_eq!(rl.record_at(&a, t0).await, RateDecision::Allow);
    }
}
