//! 同一 IP からの LOGIN 試行レート制限。
//!
//! - 直近 1 分間に 10 回（既定値）を超える LOGIN 試行があれば、以降 5 分間は拒否する。
//! - 失敗・成功を問わずカウントする（辞書攻撃の検出が目的）。
//! - 1 プロセス内の in-memory map で持つ。複数プロセスで共有する場合は別途
//!   外部ストレージ化が必要。
//!
//! **副作用（設計上の許容事項）**: 集計粒度が IP 単位のため、NAT / プロキシ / 学校・
//! 職場の共有回線配下の無関係なユーザー群が、同一出口 IP の 1 ユーザーの連続失敗に
//! 道連れで `retry_after_sec` の間ロックアウトされる。現状の運用規模と辞書攻撃検出
//! 優先の方針から本挙動は許容する。ユーザー粒度の除外が必要になった際は
//! `(IpKey, PlayerName)` 複合キーか、認証成功時に一時的な除外リストへ移す形へ
//! 拡張する。
//!
//! 各 IP の試行履歴は最大 `limit + 1` 件。失効した IP は LOGIN 時に期限索引から
//! 段階的に回収する。LOGIN がない間は保持し、1 回で処理する期限イベント数を制限する。

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rshogi_csa_server::port::RateDecision;
use rshogi_csa_server::types::IpKey;
use tokio::sync::Mutex;

// 対局時計と同じスレッドを大量の期限回収で占有しない。
const MAX_EXPIRATIONS_PER_RECORD: usize = 256;

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
    expirations: BinaryHeap<Reverse<Expiration>>,
}

struct AttemptState {
    /// 試行時刻。`window` を超えた古いものは次の試行で捨てる。
    timestamps: Vec<Instant>,
    /// 拒否期間中なら、いつまで拒否するか。
    blocked_until: Option<Instant>,
}

impl AttemptState {
    fn expires_at(&self, window: Duration) -> Option<Instant> {
        // Mutex 取得前に採時するので、保存順が時刻順になるとは限らない。
        self.blocked_until.or_else(|| self.timestamps.iter().max()?.checked_add(window))
    }
}

#[derive(PartialEq, Eq)]
struct Expiration {
    at: Instant,
    ip: IpKey,
}

impl Ord for Expiration {
    fn cmp(&self, other: &Self) -> Ordering {
        self.at.cmp(&other.at).then_with(|| self.ip.as_str().cmp(other.ip.as_str()))
    }
}

impl PartialOrd for Expiration {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Inner {
    fn prune_expired(&mut self, now: Instant, window: Duration) {
        let mut removed = false;
        // window ちょうどの試行はまだ有効なので、期限と同時刻では回収しない。
        for _ in 0..MAX_EXPIRATIONS_PER_RECORD {
            if !self.expirations.peek().is_some_and(|entry| entry.0.at < now) {
                break;
            }
            let Some(Reverse(expiration)) = self.expirations.pop() else {
                break;
            };
            let Some(state) = self.attempts.get(&expiration.ip) else {
                continue;
            };
            match state.expires_at(window) {
                Some(at) if at < now => {
                    self.attempts.remove(&expiration.ip);
                    removed = true;
                }
                Some(at) => self.expirations.push(Reverse(Expiration {
                    at,
                    ip: expiration.ip,
                })),
                None => {}
            }
        }
        if removed {
            if self.attempts.is_empty() {
                self.attempts = HashMap::new();
            } else if self.attempts.capacity() > self.attempts.len().saturating_mul(4).max(64) {
                self.attempts.shrink_to(self.attempts.len().saturating_mul(2));
            }
            if self.expirations.is_empty() {
                self.expirations = BinaryHeap::new();
            } else if self.expirations.capacity() > self.expirations.len().saturating_mul(4).max(64)
            {
                self.expirations.shrink_to(self.expirations.len().saturating_mul(2));
            }
        }
    }
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
                expirations: BinaryHeap::new(),
            })),
            window,
            penalty,
            limit,
        }
    }

    /// LOGIN 試行を 1 回カウントし、判定を返す。
    ///
    /// 呼び出し側は [`RateDecision::Deny`] の場合 LOGIN 応答として拒否を送ってから
    /// 接続を閉じること（rate deny でも CSA コマンドエラーではなくソケット切断で良い。
    /// CSA 仕様 1.2.1 には rate limit の通知コードがないため）。
    pub async fn record(&self, ip: &IpKey) -> RateDecision {
        self.record_at(ip, Instant::now()).await
    }

    /// 任意の基準時刻で判定するテスト用の内部エンドポイント。
    async fn record_at(&self, ip: &IpKey, now: Instant) -> RateDecision {
        let mut guard = self.inner.lock().await;
        guard.prune_expired(now, self.window);
        let is_new = !guard.attempts.contains_key(ip);
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

        // 2. 窓外の古い試行を捨てる。
        let threshold = now.checked_sub(self.window).unwrap_or(now);
        state.timestamps.retain(|t| *t >= threshold);

        // 3. 今回の試行を追加。
        state.timestamps.push(now);

        // 4. 窓内の件数が limit を超えていればペナルティ期間を開始。
        let decision = if state.timestamps.len() > self.limit {
            state.blocked_until = Some(now + self.penalty);
            RateDecision::Deny {
                retry_after_sec: self.penalty.as_secs().max(1),
            }
        } else {
            RateDecision::Allow
        };
        // 既存 IP の期限延長は、登録済みイベントの到来時に再評価する。
        if is_new && let Some(at) = state.expires_at(self.window) {
            guard.expirations.push(Reverse(Expiration { at, ip: ip.clone() }));
        }
        decision
    }

    /// テスト・運用での明示的なクリーンアップ（全 IP 状態を破棄）。
    pub async fn clear_all(&self) {
        let mut guard = self.inner.lock().await;
        guard.attempts = HashMap::new();
        guard.expirations = BinaryHeap::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpKey {
        IpKey::new(s)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn denies_after_limit_exceeded() {
        let rl =
            IpLoginRateLimiter::with_limits(3, Duration::from_secs(60), Duration::from_secs(300));
        let a = ip("10.0.0.2");
        let now = Instant::now();
        for _ in 0..3 {
            assert_eq!(rl.record_at(&a, now).await, RateDecision::Allow);
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

    #[tokio::test(flavor = "current_thread")]
    async fn window_boundary_keeps_attempt_until_strictly_after_expiration() {
        let rl =
            IpLoginRateLimiter::with_limits(1, Duration::from_secs(60), Duration::from_secs(300));
        let t0 = Instant::now();
        let a = ip("192.0.2.1");
        rl.record_at(&a, t0).await;
        rl.record_at(&ip("192.0.2.2"), t0 + Duration::from_secs(60)).await;
        assert!(rl.inner.lock().await.attempts.contains_key(&a));
        assert!(matches!(
            rl.record_at(&a, t0 + Duration::from_secs(60)).await,
            RateDecision::Deny { .. }
        ));

        let b = ip("192.0.2.3");
        rl.record_at(&b, t0).await;
        rl.record_at(&ip("192.0.2.4"), t0 + Duration::from_secs(60) + Duration::from_nanos(1))
            .await;
        assert!(!rl.inner.lock().await.attempts.contains_key(&b));
        assert_eq!(rl.record_at(&b, t0 + Duration::from_secs(61)).await, RateDecision::Allow);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn penalty_boundary_resets_attempts_even_when_window_is_longer() {
        for (window, penalty) in [(60, 300), (300, 60)] {
            let rl = IpLoginRateLimiter::with_limits(
                1,
                Duration::from_secs(window),
                Duration::from_secs(penalty),
            );
            let t0 = Instant::now();
            let a = ip("192.0.2.1");
            rl.record_at(&a, t0).await;
            assert!(matches!(rl.record_at(&a, t0).await, RateDecision::Deny { .. }));
            let until = t0 + Duration::from_secs(penalty);
            rl.record_at(&ip("192.0.2.2"), until - Duration::from_nanos(1)).await;
            assert!(matches!(
                rl.record_at(&a, until - Duration::from_nanos(1)).await,
                RateDecision::Deny { .. }
            ));
            assert_eq!(rl.record_at(&a, until).await, RateDecision::Allow);
            assert!(matches!(rl.record_at(&a, until).await, RateDecision::Deny { .. }));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn active_ip_is_rescheduled_without_growing_expiration_index() {
        let rl =
            IpLoginRateLimiter::with_limits(10, Duration::from_secs(60), Duration::from_secs(300));
        let t0 = Instant::now();
        let a = ip("192.0.2.1");
        let b = ip("192.0.2.2");
        rl.record_at(&a, t0).await;
        rl.record_at(&b, t0).await;
        for seconds in [10, 20, 30, 40, 50] {
            assert_eq!(
                rl.record_at(&a, t0 + Duration::from_secs(seconds)).await,
                RateDecision::Allow
            );
            assert_eq!(rl.inner.lock().await.expirations.len(), 2);
        }
        rl.record_at(&ip("192.0.2.3"), t0 + Duration::from_secs(61)).await;
        let guard = rl.inner.lock().await;
        assert!(guard.attempts.contains_key(&a));
        assert!(!guard.attempts.contains_key(&b));
        assert_eq!(guard.attempts.len(), 2);
        assert_eq!(guard.expirations.len(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reordered_timestamps_do_not_expire_latest_attempt_early() {
        let rl =
            IpLoginRateLimiter::with_limits(10, Duration::from_secs(60), Duration::from_secs(300));
        let t0 = Instant::now();
        let a = ip("192.0.2.1");
        for seconds in [100, 110, 105] {
            assert_eq!(
                rl.record_at(&a, t0 + Duration::from_secs(seconds)).await,
                RateDecision::Allow
            );
        }
        rl.record_at(&ip("192.0.2.2"), t0 + Duration::from_secs(166)).await;
        assert!(rl.inner.lock().await.attempts.contains_key(&a));
        rl.record_at(&ip("192.0.2.3"), t0 + Duration::from_secs(171)).await;
        assert!(!rl.inner.lock().await.attempts.contains_key(&a));
        assert_eq!(rl.record_at(&a, t0 + Duration::from_secs(171)).await, RateDecision::Allow);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_time_ip_burst_releases_entries_and_retained_capacity() {
        let rl =
            IpLoginRateLimiter::with_limits(10, Duration::from_secs(60), Duration::from_secs(300));
        let t0 = Instant::now();
        for n in 0..250_000 {
            let address = ip(&format!("2001:db8::{:x}:{:x}", n >> 16, n & 0xffff));
            assert_eq!(rl.record_at(&address, t0).await, RateDecision::Allow);
        }
        assert_eq!(rl.inner.lock().await.attempts.len(), 250_000);
        let current = ip("192.0.2.1");
        rl.record_at(&current, t0 + Duration::from_secs(50)).await;
        let next = ip("192.0.2.2");
        assert_eq!(rl.record_at(&next, t0 + Duration::from_secs(61)).await, RateDecision::Allow);
        assert_eq!(rl.inner.lock().await.attempts.len(), 250_000 + 2 - MAX_EXPIRATIONS_PER_RECORD,);
        for _ in 0..250_000_usize.div_ceil(MAX_EXPIRATIONS_PER_RECORD) {
            rl.record_at(&next, t0 + Duration::from_secs(61)).await;
        }
        let guard = rl.inner.lock().await;
        assert_eq!(guard.attempts.len(), 2);
        assert!(guard.attempts.contains_key(&current));
        assert!(guard.attempts.capacity() <= 64, "{}", guard.attempts.capacity());
        assert_eq!(guard.expirations.len(), 2);
        assert!(guard.expirations.capacity() <= 64, "{}", guard.expirations.capacity());
        drop(guard);
        rl.clear_all().await;
        let guard = rl.inner.lock().await;
        assert_eq!(guard.attempts.capacity(), 0);
        assert_eq!(guard.expirations.capacity(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn expiration_budget_also_limits_rescheduling_of_active_ips() {
        let rl = IpLoginRateLimiter::default_limits();
        let t0 = Instant::now();
        let count = MAX_EXPIRATIONS_PER_RECORD * 3;
        for n in 0..count {
            let address = ip(&format!("2001:db8::{n:x}"));
            rl.record_at(&address, t0).await;
            rl.record_at(&address, t0 + Duration::from_secs(50)).await;
        }
        rl.record_at(&ip("192.0.2.1"), t0 + Duration::from_secs(61)).await;
        let guard = rl.inner.lock().await;
        assert_eq!(guard.attempts.len(), count + 1);
        assert_eq!(guard.expirations.len(), count + 1);
        assert_eq!(
            guard
                .expirations
                .iter()
                .filter(|event| event.0.at == t0 + Duration::from_secs(110))
                .count(),
            MAX_EXPIRATIONS_PER_RECORD,
        );
        assert_eq!(
            guard
                .expirations
                .iter()
                .filter(|event| event.0.at == t0 + Duration::from_secs(60))
                .count(),
            count - MAX_EXPIRATIONS_PER_RECORD,
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stale_current_ip_uses_window_and_penalty_while_cleanup_is_backlogged() {
        for blocked in [false, true] {
            let rl = IpLoginRateLimiter::with_limits(
                2,
                Duration::from_secs(60),
                Duration::from_secs(300),
            );
            let t0 = Instant::now();
            let target = ip("ffff::1");
            rl.record_at(&target, t0).await;
            rl.record_at(&target, t0).await;
            if blocked {
                assert!(matches!(rl.record_at(&target, t0).await, RateDecision::Deny { .. }));
            }
            for n in 0..MAX_EXPIRATIONS_PER_RECORD * 10 {
                rl.record_at(&ip(&format!("2001:db8::{n:x}")), t0).await;
            }
            let now = t0 + Duration::from_secs(if blocked { 301 } else { 61 });
            assert_eq!(rl.record_at(&target, now).await, RateDecision::Allow);
            assert_eq!(rl.record_at(&target, now).await, RateDecision::Allow);
            assert!(matches!(rl.record_at(&target, now).await, RateDecision::Deny { .. }));
            let guard = rl.inner.lock().await;
            assert!(guard.expirations.iter().any(|event| event.0.ip == target && event.0.at < now));
            assert_eq!(guard.expirations.len(), guard.attempts.len());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn repeated_unique_ip_waves_remain_bounded_by_live_wave() {
        let rl = IpLoginRateLimiter::default_limits();
        let t0 = Instant::now();
        for wave in 0..20 {
            let now = t0 + Duration::from_secs(wave * 61);
            for n in 0..2000 {
                let address = ip(&format!("2001:db8:{wave:x}::{n:x}"));
                assert_eq!(rl.record_at(&address, now).await, RateDecision::Allow);
            }
            let guard = rl.inner.lock().await;
            assert_eq!(guard.attempts.len(), 2000);
            assert_eq!(guard.expirations.len(), 2000);
        }
    }
}
