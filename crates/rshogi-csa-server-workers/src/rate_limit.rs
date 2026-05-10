//! Rate limit / abuse protection (issue #622 PR3a)。
//!
//! Floodgate 相当 production 昇格 blocker。`/ws/lobby` への LOGIN_LOBBY /
//! CHALLENGE_LOBBY flood、`/ws/<room_id>` upgrade flood、room 起動 flood を
//! Worker code 側の **atomic token bucket** で抑制する。Cloudflare Workers
//! Rate Limiting binding は本アカウントで利用不可確認のため、専用 Durable
//! Object (`RateLimiterDO`) を per-key sharding で実装する (Q2-B 採択、
//! 2026-05-10 user 確認、`docs/csa-server/rate_limit_design.md` §3 Q2)。
//!
//! # 設計の前提
//!
//! - **CF-Connecting-IP**: クライアント IP は本ヘッダ "のみ" を信頼する
//!   (`X-Forwarded-For` は Cloudflare 通過後も client が偽装可能)。
//!   ヘッダ欠落時は **fail-closed** で 503 + `Retry-After` 短時間。
//! - **DO sharding**: 1 (kind, identifier) = 1 DO instance
//!   (`id_from_name(format!("{kind_tag}:{identifier}"))`)。Cloudflare 側で
//!   per-key 自然 sharding が成立し、idle DO は GC される。
//! - **token bucket**: capacity = limit/min, refill rate = limit/60 token/sec。
//!   満タンからの burst を許容しつつ、長期平均は X/min を超えない。
//! - **永続化**: DO storage に `{tokens, last_refill_ms}` を毎 check 後 put。
//!   instance eviction で in-memory state が失われても 1 isolate あたり最大
//!   1 capacity 分の over-allow に収まる (= 安全側)。
//! - **WS upgrade hot path**: `accept_web_socket` 直前で発火するため、
//!   ヒープ割り当てを最小化する (Vec / HashMap clone 禁止)。
//!
//! 詳細は `docs/csa-server/rate_limit_design.md` 参照。

use serde::{Deserialize, Serialize};

#[cfg(target_arch = "wasm32")]
use worker::{
    Date, DurableObject, Env, Error, Headers, Method, Request, Response, ResponseBuilder,
    Result as WorkerResult, State, durable_object, wasm_bindgen,
};

#[cfg(target_arch = "wasm32")]
use crate::config::ConfigKeys;

/// Rate limit を適用する文脈。各 variant が 1 個の env 閾値に対応する。
///
/// `kind_tag` は DO ID 名前空間の prefix として使う (`"{kind_tag}:{identifier}"`)。
/// 異なる kind の同 identifier は別 DO instance として扱われる (= bucket 干渉なし)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitKind {
    /// `LOGIN_LOBBY` (公開 + 私的) per source IP。
    LobbyLoginPerIp,
    /// `LOGIN_LOBBY` (公開) per claimed handle。
    LobbyLoginPerHandle,
    /// `CHALLENGE_LOBBY` per source IP。
    LobbyChallengePerIp,
    /// `CHALLENGE_LOBBY` per inviter handle。
    LobbyChallengePerInviter,
    /// `/ws/<room_id>` 上で GameRoom DO が起動 (= player route upgrade) per source IP。
    RoomCreatePerIp,
    /// `/ws/<room_id>(/spectate)` upgrade 全般 per source IP。
    /// player / spectator の両 route が共有する WS upgrade 流量上限。
    WsRoomUpgradePerIp,
}

impl RateLimitKind {
    /// DO ID 名前空間の prefix。同 identifier でも kind が異なれば別 DO に解決される。
    pub const fn kind_tag(self) -> &'static str {
        match self {
            Self::LobbyLoginPerIp => "lobby_login_ip",
            Self::LobbyLoginPerHandle => "lobby_login_handle",
            Self::LobbyChallengePerIp => "lobby_challenge_ip",
            Self::LobbyChallengePerInviter => "lobby_challenge_inviter",
            Self::RoomCreatePerIp => "room_create_ip",
            Self::WsRoomUpgradePerIp => "ws_upgrade_ip",
        }
    }
}

/// `ConfigKeys::*_RATE_PER_*_PER_MIN` から解決した 6 個の閾値。
/// 各値は `0` を「無効化」と解釈せず、`fallback` (= 既定値) にフォールバックする
/// (env 設定不正で全リクエストが拒否される事故を避ける安全側挙動)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitThresholds {
    pub lobby_login_per_ip: u32,
    pub lobby_login_per_handle: u32,
    pub lobby_challenge_per_ip: u32,
    pub lobby_challenge_per_inviter: u32,
    pub room_create_per_ip: u32,
    pub ws_room_upgrade_per_ip: u32,
}

impl RateLimitThresholds {
    /// 設計 doc §3 Q3 表の推奨初期値 (2026-05-10 user 確定)。env 未設定 / 不正値の
    /// fallback として参照する。
    pub const DEFAULTS: Self = Self {
        lobby_login_per_ip: 10,
        lobby_login_per_handle: 5,
        lobby_challenge_per_ip: 5,
        lobby_challenge_per_inviter: 3,
        room_create_per_ip: 20,
        ws_room_upgrade_per_ip: 60,
    };

    /// 個別 kind に対応する閾値を返す。
    pub fn limit_for(&self, kind: RateLimitKind) -> u32 {
        match kind {
            RateLimitKind::LobbyLoginPerIp => self.lobby_login_per_ip,
            RateLimitKind::LobbyLoginPerHandle => self.lobby_login_per_handle,
            RateLimitKind::LobbyChallengePerIp => self.lobby_challenge_per_ip,
            RateLimitKind::LobbyChallengePerInviter => self.lobby_challenge_per_inviter,
            RateLimitKind::RoomCreatePerIp => self.room_create_per_ip,
            RateLimitKind::WsRoomUpgradePerIp => self.ws_room_upgrade_per_ip,
        }
    }
}

/// 文字列を「正の u32 (1..=u32::MAX)」として解釈する pure helper。`None` /
/// 空文字 / `0` / 非数値 / 範囲外は `fallback` を返す。
///
/// `0` を fallback に倒すのは「env 設定で 0 を入れて運用を無効化する」と
/// 「typo / 範囲外で 0 になる」の区別が付かないため。本 PR では「rate limit
/// は常に有効」を staging / production 共通の不変条件として固定し、
/// env 経由で無効化する経路を提供しない (運用で外したくなった場合は
/// 巨大な値を入れて事実上無効化する設計に倒す)。
pub fn resolve_positive_u32_threshold(raw: Option<&str>, fallback: u32) -> u32 {
    let trimmed = raw.unwrap_or("").trim();
    if trimmed.is_empty() {
        return fallback;
    }
    match trimmed.parse::<u32>() {
        Ok(0) | Err(_) => fallback,
        Ok(v) => v,
    }
}

/// `wasm32` ランタイムで `worker::Env` から 6 個の閾値を解決する。
#[cfg(target_arch = "wasm32")]
pub fn resolve_thresholds_from_env(env: &Env) -> RateLimitThresholds {
    fn read(env: &Env, key: &str) -> Option<String> {
        env.var(key).ok().map(|v| v.to_string())
    }
    let d = RateLimitThresholds::DEFAULTS;
    RateLimitThresholds {
        lobby_login_per_ip: resolve_positive_u32_threshold(
            read(env, ConfigKeys::LOBBY_LOGIN_RATE_PER_IP_PER_MIN).as_deref(),
            d.lobby_login_per_ip,
        ),
        lobby_login_per_handle: resolve_positive_u32_threshold(
            read(env, ConfigKeys::LOBBY_LOGIN_RATE_PER_HANDLE_PER_MIN).as_deref(),
            d.lobby_login_per_handle,
        ),
        lobby_challenge_per_ip: resolve_positive_u32_threshold(
            read(env, ConfigKeys::LOBBY_CHALLENGE_RATE_PER_IP_PER_MIN).as_deref(),
            d.lobby_challenge_per_ip,
        ),
        lobby_challenge_per_inviter: resolve_positive_u32_threshold(
            read(env, ConfigKeys::LOBBY_CHALLENGE_RATE_PER_HANDLE_PER_MIN).as_deref(),
            d.lobby_challenge_per_inviter,
        ),
        room_create_per_ip: resolve_positive_u32_threshold(
            read(env, ConfigKeys::ROOM_CREATE_RATE_PER_IP_PER_MIN).as_deref(),
            d.room_create_per_ip,
        ),
        ws_room_upgrade_per_ip: resolve_positive_u32_threshold(
            read(env, ConfigKeys::WS_ROOM_UPGRADE_RATE_PER_IP_PER_MIN).as_deref(),
            d.ws_room_upgrade_per_ip,
        ),
    }
}

/// 1 bucket の判定結果。`allowed = false` のとき `retry_after` は **次の 1 token が
/// refill されるまで** の秒数 (常に >= 1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitDecision {
    pub allowed: bool,
    /// `Retry-After` ヘッダや `retry_after=<sec>` 行に埋める秒数。
    /// `allowed = true` のとき `0`、`false` のとき `>= 1`。
    pub retry_after_sec: u64,
}

impl RateLimitDecision {
    pub const fn allow() -> Self {
        Self {
            allowed: true,
            retry_after_sec: 0,
        }
    }

    pub const fn deny(retry_after_sec: u64) -> Self {
        // `retry_after_sec = 0` だと client が即時 retry して storm を起こすため、
        // `>= 1` を不変条件として固定する。
        let safe = if retry_after_sec == 0 {
            1
        } else {
            retry_after_sec
        };
        Self {
            allowed: false,
            retry_after_sec: safe,
        }
    }
}

/// 1 bucket の永続状態。`tokens` は `f64` で部分 token を保持し、refill の精度を
/// 1 ms 粒度で確保する。
///
/// SerDe 経由で DO storage / DO RPC body の双方に流すため `Serialize +
/// Deserialize` を実装する。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TokenBucketState {
    /// 現在の token 残量 (0.0..=capacity)。capacity 超過は `refill` で clamp される。
    pub tokens: f64,
    /// 最後に refill / consume した壁時計 ms (UNIX epoch、`worker::Date::now()`)。
    /// `now_ms < last_refill_ms` (時刻巻き戻り) の場合は `refill` で `last_refill_ms`
    /// を `now_ms` に揃えるだけにとどめ、tokens は触らない。
    pub last_refill_ms: u64,
}

impl TokenBucketState {
    /// 満 capacity で初期化した bucket を返す。新規 DO instance / cold start で使う。
    pub fn full(capacity: u32, now_ms: u64) -> Self {
        Self {
            tokens: f64::from(capacity),
            last_refill_ms: now_ms,
        }
    }

    /// `last_refill_ms` から `now_ms` までの経過時間に応じて token を refill する。
    /// refill rate = `capacity / 60` token/sec (= 60 秒で 1 capacity 分回復)。
    /// `tokens` は `capacity` で clamp する。
    ///
    /// `now_ms <= last_refill_ms` (時刻巻き戻り、isolate 移動時に観測しうる、
    /// および同 ms 内連続 check) では refill を行わず `last_refill_ms = now_ms` だけ
    /// 更新する (= 過剰 refill を防ぐ。等値時は `elapsed_ms = 0` で分岐しなくても
    /// 結果は変わらないが、`> 0` 経路に統一して算術 overflow / 浮動小数誤差の
    /// 余地を排除する)。
    pub fn refill(&mut self, capacity: u32, now_ms: u64) {
        if capacity == 0 {
            // capacity = 0 は env 解決時に fallback で弾いているはずだが、防御的に
            // refill を no-op にして 0/0 計算を回避する。
            self.last_refill_ms = now_ms;
            return;
        }
        if now_ms <= self.last_refill_ms {
            self.last_refill_ms = now_ms;
            return;
        }
        let elapsed_ms = now_ms - self.last_refill_ms;
        // refill = elapsed_sec * (capacity / 60) = elapsed_ms * capacity / 60_000
        let refill = (elapsed_ms as f64) * f64::from(capacity) / 60_000.0;
        let cap = f64::from(capacity);
        self.tokens = (self.tokens + refill).min(cap);
        self.last_refill_ms = now_ms;
    }

    /// 1 token を消費しようとする。残量が 1 以上なら消費して `allow`、不足なら
    /// 残量に基づいた `deny(retry_after_sec)` を返す。
    ///
    /// `retry_after_sec`: 残量が 1 token に達するまでの秒数を **天井で切り上げ**
    /// (ceil) して返す。`Retry-After: 0` を返すと client が即時 retry して
    /// race するため、ceil + 最小 1 秒で client backoff を保証する。
    pub fn try_consume(&mut self, capacity: u32, now_ms: u64) -> RateLimitDecision {
        self.refill(capacity, now_ms);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return RateLimitDecision::allow();
        }
        // refill 速度 = capacity / 60 token/sec。
        // 1 token に達するまでの秒数 = (1 - tokens) * 60 / capacity。
        // capacity = 0 は refill 側で弾いているので分岐不要。
        let needed = (1.0 - self.tokens).max(0.0);
        let retry_sec = (needed * 60.0 / f64::from(capacity)).ceil() as u64;
        RateLimitDecision::deny(retry_sec)
    }
}

// --- DO RPC wire format ------------------------------------------------------

/// `RateLimiter::fetch` の query string で使う `capacity` 引数キー。
///
/// caller (Worker fetch handler) が env から解決した token 上限値を URL 経由で
/// 引き渡す。DO 側が env を読まない設計にすることで、閾値変更を deploy なしで
/// 反映できる経路を保つ (= env 値が次回 check で即反映)。POST body / JSON では
/// なく query string を使うのは、`worker` crate の `RequestInit::with_body` が
/// `wasm_bindgen::JsValue` を要求し本 crate の依存に wasm-bindgen を追加せずに
/// 済ませるための判断 (= boilerplate と依存表面を縮める)。
pub const CHECK_QUERY_CAPACITY_KEY: &str = "capacity";

// --- helpers used by both wasm32 and host tests ------------------------------

/// `(kind, identifier)` から DO `id_from_name` 用のキーを組み立てる。
///
/// 不変条件:
/// - `identifier` は CF-Connecting-IP もしくは LOGIN handle の生文字列。空文字は
///   caller (extract_client_ip 等) が `None` 化することで本関数まで到達させない契約。
/// - 結果文字列は `id_from_name` (= 任意 UTF-8) に渡せれば衝突しない。`:` で kind と
///   identifier を分離するが、handle 側の `:` は CSA プロトコル上ほぼ無いと想定して
///   そのまま埋め込む (識別子衝突しても rate limit 副作用は同 IP / handle 系列内に
///   留まる安全側ミス)。
pub fn build_do_id_name(kind: RateLimitKind, identifier: &str) -> String {
    let tag = kind.kind_tag();
    let mut s = String::with_capacity(tag.len() + 1 + identifier.len());
    s.push_str(tag);
    s.push(':');
    s.push_str(identifier);
    s
}

// --- WS upgrade fail-closed helpers ------------------------------------------

/// `LOGIN_LOBBY:incorrect rate_limited retry_after=<sec>` の body 1 行を組み立てる。
/// `lobby_protocol::build_login_incorrect_line` と同型を維持しつつ、`retry_after`
/// 値を含める専用の helper として分離する (本ヘルパは rate_limit 文脈でしか使わ
/// ないため protocol 側を汚染しない)。
pub fn build_login_lobby_rate_limited_line(retry_after_sec: u64) -> String {
    format!("LOGIN_LOBBY:incorrect rate_limited retry_after={retry_after_sec}")
}

/// `CHALLENGE_LOBBY:incorrect rate_limited retry_after=<sec>` の body 1 行。
pub fn build_challenge_lobby_rate_limited_line(retry_after_sec: u64) -> String {
    format!("CHALLENGE_LOBBY:incorrect rate_limited retry_after={retry_after_sec}")
}

/// CF-Connecting-IP fail-closed のとき返す short retry (= 10 秒)。
/// design doc §2.3 で「正規経路で常に存在するヘッダなので欠落は anomalous、
/// 短時間で client を bounce する」契約 (`Retry-After: 10`)。
pub const FAIL_CLOSED_MISSING_IP_RETRY_AFTER_SEC: u64 = 10;

// --- wasm32-only: extract IP, call DO, build HTTP response -------------------

/// `CF-Connecting-IP` ヘッダから client IP を取り出す。Cloudflare 経由の正規
/// request では常に存在するヘッダで、欠落 / 空文字は anomalous (= fail-closed)。
///
/// `X-Forwarded-For` は client が偽装可能なため意図的に **使わない**
/// (`docs/csa-server/rate_limit_design.md` §2.3 共通実装ガード参照)。
#[cfg(target_arch = "wasm32")]
pub fn extract_client_ip(req: &Request) -> Option<String> {
    let raw = req.headers().get("CF-Connecting-IP").ok().flatten()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}

/// `RateLimiterDO` に対して check & consume RPC を 1 回投げ、判定を返す。
///
/// `identifier` には IP アドレス (CF-Connecting-IP) もしくは LOGIN handle の生
/// 文字列を渡す。`capacity` は env から都度解決した値を渡す (DO は env を読まない、
/// `CheckAndConsumeRequest::capacity` の docstring 参照)。
///
/// **エラーセマンティクス**: DO RPC 自体に失敗した場合 (`stub.fetch_with_request`
/// が `Err` を返した場合) は呼び出し側で fail-closed (= deny) させるため、
/// `Result::Err` を伝播する。これにより transient な DO 障害でも rate limiter が
/// 「全 allow」に倒れる経路を作らない (security design 観点で fail-closed を選ぶ)。
#[cfg(target_arch = "wasm32")]
pub async fn check_and_consume_via_do(
    env: &Env,
    kind: RateLimitKind,
    identifier: &str,
    capacity: u32,
) -> WorkerResult<RateLimitDecision> {
    let namespace = env.durable_object(ConfigKeys::RATE_LIMITER_BINDING)?;
    let id_name = build_do_id_name(kind, identifier);
    let stub = namespace.id_from_name(&id_name)?.get_stub()?;

    // `capacity` を query string で渡す (`POST /check?capacity=N`)。body / JSON
    // 経路を採らない理由は `CHECK_QUERY_CAPACITY_KEY` の docstring 参照。
    // host 部分は `do.internal` 系の dummy で OK (DO 側で host は無視する)。
    let url = format!(
        "https://do.internal/check_and_consume?{key}={cap}",
        key = CHECK_QUERY_CAPACITY_KEY,
        cap = capacity,
    );
    let req = Request::new(&url, Method::Post)?;
    let mut resp = stub.fetch_with_request(req).await?;
    if resp.status_code() != 200 {
        let status = resp.status_code();
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::RustError(format!(
            "rate_limit DO returned non-200: status={status} body={body}"
        )));
    }
    let decision: RateLimitDecision = resp
        .json()
        .await
        .map_err(|e| Error::RustError(format!("rate_limit DO response not JSON: {e}")))?;
    Ok(decision)
}

// --- DurableObject implementation --------------------------------------------

/// 1 bucket = 1 DO instance な atomic token bucket DO。`id_from_name` で
/// `(kind, identifier)` 単位に分散し、Cloudflare DO の単一スレッド実行モデルで
/// `refill → try_consume → persist` の atomicity を担保する。
///
/// **持ち物**:
/// - `state.storage().get(KEY_BUCKET)`: `TokenBucketState` を JSON で永続化。
///   instance eviction で in-memory が消えても次起動で復元する。
/// - `bucket: RefCell<Option<TokenBucketState>>`: 同 instance への 2 回目以降の
///   request で storage round-trip を回避する lazy cache。
///
/// **wire format** (本 DO への RPC):
/// - `POST <any-path>` body = `CheckAndConsumeRequest` JSON
/// - 戻り値 200: `RateLimitDecision` JSON (`{allowed, retry_after_sec}`)
/// - 戻り値 4xx/5xx: caller 側で fail-closed (deny) するか propagate するかを選ぶ
#[cfg(target_arch = "wasm32")]
#[durable_object]
pub struct RateLimiter {
    state: State,
    /// 同 instance 内の lazy cache。`fetch` 1 回目は storage から load し、
    /// 以降の fetch は本 cell から取り出す。`websocket_*` ハンドラは持たないため
    /// 同 instance 上での fetch 連発のみが想定される並行経路で、Cloudflare DO の
    /// 単一スレッドモデルにより `RefCell::borrow_mut` の panic 経路は発生しない。
    bucket: std::cell::RefCell<Option<TokenBucketState>>,
}

/// DO storage の bucket key。1 DO instance に 1 bucket のみ保持するので単一 key
/// で衝突しない。
#[cfg(target_arch = "wasm32")]
const KEY_BUCKET: &str = "bucket";

#[cfg(target_arch = "wasm32")]
impl DurableObject for RateLimiter {
    fn new(state: State, _env: Env) -> Self {
        Self {
            state,
            bucket: std::cell::RefCell::new(None),
        }
    }

    async fn fetch(&self, req: Request) -> WorkerResult<Response> {
        // POST のみ受理。GET / その他は 405 (`docs/csa-server/rate_limit.md` 参照)。
        if req.method() != Method::Post {
            return Response::error("rate_limit DO: POST only", 405);
        }

        // `?capacity=N` を query から取り出す。`url::Url::query_pairs` は
        // worker::Url と同等の API を提供する (`worker::Url` は `url::Url` の
        // re-export)。capacity 不在 / 非数値 / 0 は即 deny に倒し、bucket 状態を
        // 動かさず短時間 retry を返す (= caller 側 env 解決の安全網)。
        let url = req.url()?;
        let capacity: u32 = url
            .query_pairs()
            .find(|(k, _)| k == CHECK_QUERY_CAPACITY_KEY)
            .and_then(|(_, v)| v.parse::<u32>().ok())
            .unwrap_or(0);
        if capacity == 0 {
            let decision = RateLimitDecision::deny(FAIL_CLOSED_MISSING_IP_RETRY_AFTER_SEC);
            return response_json(&decision);
        }

        let now_ms = Date::now().as_millis();

        // lazy load。1 instance 1 回目だけ storage round-trip。
        // `get` の戻り値を 3 経路で扱い、`Err` (transient storage 障害) は
        // **fail-closed** に倒す:
        // - `Ok(Some(state))` → 既存 bucket を復元
        // - `Ok(None)`        → 真の cold start (= 当該 (kind, identifier) で
        //                       初めての check)。満タン bucket で初期化して allow
        //                       経路に乗る
        // - `Err(_)`          → DO storage 一時障害 / ネットワーク。`Ok(None)` と
        //                       一緒くたにすると過去残量を失った満タン bucket で
        //                       allow 連発になり rate limit 緩和の脆弱性になる。
        //                       fail-closed の deny を返し、in-memory cache も
        //                       汚さない (次回 fetch で再 `get` を試みる)。
        let mut cell = self.bucket.borrow_mut();
        if cell.is_none() {
            let loaded: Option<TokenBucketState> =
                match self.state.storage().get::<TokenBucketState>(KEY_BUCKET).await {
                    Ok(v) => v,
                    Err(_) => {
                        // cell は触らずに deny を返して終了。次の fetch で再 lazy-load
                        // を試みる (transient なら回復、永続障害なら毎回 deny で
                        // safety を保つ)。
                        let decision =
                            RateLimitDecision::deny(FAIL_CLOSED_MISSING_IP_RETRY_AFTER_SEC);
                        return response_json(&decision);
                    }
                };
            *cell = Some(loaded.unwrap_or_else(|| TokenBucketState::full(capacity, now_ms)));
        }
        let bucket = cell.as_mut().expect("lazy-init above");

        let decision = bucket.try_consume(capacity, now_ms);

        // persist。in-memory state と storage を一致させる (eviction 対策)。
        // `put` 失敗は `?` で上位伝播 → caller (Worker fetch handler) 側で
        // fail-closed (Err = deny) になる (router / lobby ハンドラで `?` 伝播)。
        self.state.storage().put(KEY_BUCKET, bucket).await?;

        response_json(&decision)
    }
}

#[cfg(target_arch = "wasm32")]
fn response_json<T: Serialize>(value: &T) -> WorkerResult<Response> {
    let body = serde_json::to_string(value)
        .map_err(|e| Error::RustError(format!("rate_limit DO serialize: {e}")))?;
    let headers = Headers::new();
    let _ = headers.set("content-type", "application/json");
    let resp = ResponseBuilder::new()
        .with_status(200)
        .with_headers(headers)
        .from_bytes(body.into_bytes())?;
    Ok(resp)
}

// --- HTTP fail-closed builder for /ws/<room_id> upgrade path -----------------

/// `/ws/<room_id>(/spectate)` upgrade 経路で rate limit 拒否を 503 + Retry-After
/// で返す。本 helper は `Response` 自体を組むため wasm32 のみ。
#[cfg(target_arch = "wasm32")]
pub fn build_ws_upgrade_rate_limited_response(retry_after_sec: u64) -> WorkerResult<Response> {
    let body = format!(
        "rate_limited; retry after {retry_after_sec} seconds (issue #622, see docs/csa-server/rate_limit.md)"
    );
    let headers = Headers::new();
    let _ = headers.set("Retry-After", &retry_after_sec.to_string());
    let _ = headers.set("content-type", "text/plain; charset=utf-8");
    let resp = ResponseBuilder::new()
        .with_status(503)
        .with_headers(headers)
        .from_bytes(body.into_bytes())?;
    Ok(resp)
}

/// CF-Connecting-IP 欠落時に返す 503 (= fail-closed)。`Retry-After` は短時間
/// (10 秒) で固定する (anomalous request を想定、長時間 block すると正規 client
/// が CF 経由復帰時に長く待たされる経路ができる)。
#[cfg(target_arch = "wasm32")]
pub fn build_missing_ip_response() -> WorkerResult<Response> {
    build_ws_upgrade_rate_limited_response(FAIL_CLOSED_MISSING_IP_RETRY_AFTER_SEC)
}

// --- tests (host target) -----------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// 60_000 ms = 1 minute は capacity 全量を refill する根拠時間。本テストで
    /// 1 minute スパンの境界挙動を全部回す。
    const ONE_MINUTE_MS: u64 = 60_000;

    #[test]
    fn defaults_match_design_doc_q3_table() {
        // 設計 doc §3 Q3 表の 6 推奨値 (2026-05-10 user 確定)。
        let d = RateLimitThresholds::DEFAULTS;
        assert_eq!(d.lobby_login_per_ip, 10);
        assert_eq!(d.lobby_login_per_handle, 5);
        assert_eq!(d.lobby_challenge_per_ip, 5);
        assert_eq!(d.lobby_challenge_per_inviter, 3);
        assert_eq!(d.room_create_per_ip, 20);
        assert_eq!(d.ws_room_upgrade_per_ip, 60);
    }

    #[test]
    fn limit_for_returns_per_kind_value() {
        let t = RateLimitThresholds {
            lobby_login_per_ip: 1,
            lobby_login_per_handle: 2,
            lobby_challenge_per_ip: 3,
            lobby_challenge_per_inviter: 4,
            room_create_per_ip: 5,
            ws_room_upgrade_per_ip: 6,
        };
        assert_eq!(t.limit_for(RateLimitKind::LobbyLoginPerIp), 1);
        assert_eq!(t.limit_for(RateLimitKind::LobbyLoginPerHandle), 2);
        assert_eq!(t.limit_for(RateLimitKind::LobbyChallengePerIp), 3);
        assert_eq!(t.limit_for(RateLimitKind::LobbyChallengePerInviter), 4);
        assert_eq!(t.limit_for(RateLimitKind::RoomCreatePerIp), 5);
        assert_eq!(t.limit_for(RateLimitKind::WsRoomUpgradePerIp), 6);
    }

    #[test]
    fn resolve_threshold_handles_unset_blank_zero_invalid() {
        // `0` / 空 / 非数値 は fallback。`> 0` の正値はそのまま採用。
        assert_eq!(resolve_positive_u32_threshold(None, 10), 10);
        assert_eq!(resolve_positive_u32_threshold(Some(""), 10), 10);
        assert_eq!(resolve_positive_u32_threshold(Some("  "), 10), 10);
        assert_eq!(resolve_positive_u32_threshold(Some("0"), 10), 10);
        assert_eq!(resolve_positive_u32_threshold(Some("forever"), 10), 10);
        assert_eq!(resolve_positive_u32_threshold(Some("-5"), 10), 10);
        assert_eq!(resolve_positive_u32_threshold(Some("99999999999"), 10), 10);
        assert_eq!(resolve_positive_u32_threshold(Some("3"), 10), 3);
        assert_eq!(resolve_positive_u32_threshold(Some(" 12\n"), 10), 12);
    }

    #[test]
    fn build_do_id_name_concatenates_kind_and_identifier() {
        // identifier は IP / handle どちらも素通し。`:` 区切りで kind と
        // identifier 部を分離する。
        assert_eq!(
            build_do_id_name(RateLimitKind::LobbyLoginPerIp, "1.2.3.4"),
            "lobby_login_ip:1.2.3.4"
        );
        assert_eq!(
            build_do_id_name(RateLimitKind::LobbyLoginPerHandle, "alice"),
            "lobby_login_handle:alice"
        );
        assert_eq!(
            build_do_id_name(RateLimitKind::LobbyChallengePerIp, "[2001:db8::1]"),
            "lobby_challenge_ip:[2001:db8::1]"
        );
    }

    #[test]
    fn full_bucket_initializes_at_capacity() {
        let b = TokenBucketState::full(10, 1_000);
        assert_eq!(b.tokens, 10.0);
        assert_eq!(b.last_refill_ms, 1_000);
    }

    #[test]
    fn try_consume_succeeds_within_capacity() {
        let mut b = TokenBucketState::full(3, 0);
        for _ in 0..3 {
            assert_eq!(b.try_consume(3, 0), RateLimitDecision::allow());
        }
    }

    #[test]
    fn try_consume_denies_after_capacity_exhausted_and_returns_retry_after() {
        let mut b = TokenBucketState::full(6, 0);
        for _ in 0..6 {
            assert_eq!(b.try_consume(6, 0), RateLimitDecision::allow());
        }
        // 7 回目: bucket 空、deny。capacity=6 → 60/6 = 10 秒/token が refill 速度。
        // 必要量 = 1 token、retry_sec = ceil(1.0 * 60 / 6) = 10 秒。
        let decision = b.try_consume(6, 0);
        assert!(!decision.allowed);
        assert_eq!(decision.retry_after_sec, 10);
    }

    #[test]
    fn deny_zero_retry_after_clamps_to_one_second() {
        // `RateLimitDecision::deny(0)` は不変条件で 1 秒に切り上げる
        // (`Retry-After: 0` 返却で client storm を起こさないため)。
        let d = RateLimitDecision::deny(0);
        assert!(!d.allowed);
        assert_eq!(d.retry_after_sec, 1);
    }

    #[test]
    fn refill_recovers_one_capacity_per_minute() {
        // capacity = 60 → refill rate = 1 token/sec。10 秒で 10 token 回復。
        let mut b = TokenBucketState::full(60, 0);
        // 全消費
        for _ in 0..60 {
            assert_eq!(b.try_consume(60, 0), RateLimitDecision::allow());
        }
        // 0 token 状態から 10 秒経過 → 10 token 回復
        b.refill(60, 10_000);
        assert!((b.tokens - 10.0).abs() < 1e-6);
    }

    #[test]
    fn refill_is_clamped_at_capacity() {
        let mut b = TokenBucketState::full(10, 0);
        // 5 消費 → tokens = 5
        for _ in 0..5 {
            assert_eq!(b.try_consume(10, 0), RateLimitDecision::allow());
        }
        assert!((b.tokens - 5.0).abs() < 1e-6);
        // 1 minute 経過 → 10 token refill 候補だが capacity = 10 で clamp。
        b.refill(10, ONE_MINUTE_MS);
        assert!((b.tokens - 10.0).abs() < 1e-6);
        // さらに経過しても overflow しない。
        b.refill(10, ONE_MINUTE_MS * 10);
        assert!((b.tokens - 10.0).abs() < 1e-6);
    }

    #[test]
    fn refill_is_no_op_on_clock_rewind_or_same_ms() {
        // `now_ms <= last_refill_ms` は isolate 移動等の時刻巻き戻り、または同 ms 内
        // の連続 check で観測しうる。refill を行わず last_refill_ms だけ揃え、
        // tokens を勝手に増やさない。
        let mut b = TokenBucketState {
            tokens: 3.0,
            last_refill_ms: 10_000,
        };
        // 巻き戻り (now_ms < last_refill_ms)
        b.refill(10, 5_000);
        assert_eq!(b.tokens, 3.0);
        assert_eq!(b.last_refill_ms, 5_000);

        // 等値 (now_ms == last_refill_ms): 同 ms 内連続 check で発火する境界。
        // tokens は触らない。
        let mut b2 = TokenBucketState {
            tokens: 3.0,
            last_refill_ms: 5_000,
        };
        b2.refill(10, 5_000);
        assert_eq!(b2.tokens, 3.0);
        assert_eq!(b2.last_refill_ms, 5_000);
    }

    #[test]
    fn try_consume_with_partial_token_calculates_correct_retry_after() {
        // capacity = 6 (= LobbyChallengePerInviter * 2 想定)
        // 全消費後、6 秒経過 → 0.6 token 回復。次 1 token に達するには 0.4 token 必要。
        // retry_sec = ceil(0.4 * 60 / 6) = ceil(4.0) = 4 秒。
        let mut b = TokenBucketState::full(6, 0);
        for _ in 0..6 {
            b.try_consume(6, 0);
        }
        let decision = b.try_consume(6, 6_000);
        assert!(!decision.allowed);
        assert_eq!(decision.retry_after_sec, 4);
    }

    #[test]
    fn try_consume_resumes_after_window_recovery() {
        // 60 秒経過で capacity 全回復 → 再び capacity 回まで allow。
        let mut b = TokenBucketState::full(2, 0);
        b.try_consume(2, 0);
        b.try_consume(2, 0);
        let denied = b.try_consume(2, 0);
        assert!(!denied.allowed);
        // 60 秒後 → 全回復。2 回 allow できる。
        assert_eq!(b.try_consume(2, ONE_MINUTE_MS), RateLimitDecision::allow());
        assert_eq!(b.try_consume(2, ONE_MINUTE_MS), RateLimitDecision::allow());
    }

    #[test]
    fn build_login_lobby_rate_limited_line_format() {
        assert_eq!(
            build_login_lobby_rate_limited_line(15),
            "LOGIN_LOBBY:incorrect rate_limited retry_after=15"
        );
    }

    #[test]
    fn build_challenge_lobby_rate_limited_line_format() {
        assert_eq!(
            build_challenge_lobby_rate_limited_line(7),
            "CHALLENGE_LOBBY:incorrect rate_limited retry_after=7"
        );
    }

    #[test]
    fn fail_closed_missing_ip_retry_after_short() {
        // anomalous request (CF-Connecting-IP 欠落) は短時間で bounce する契約。
        // `Retry-After: 10` 程度の小さい値を保つ (長時間にすると CF 経由復帰時の
        // 正規 client 待ち時間を不当に伸ばす)。`const { assert!(..) }` で値変更
        // 時にコンパイル時エラーになる。
        const _: () = assert!(FAIL_CLOSED_MISSING_IP_RETRY_AFTER_SEC <= 30);
        const _: () = assert!(FAIL_CLOSED_MISSING_IP_RETRY_AFTER_SEC >= 1);
    }

    /// 6 kind それぞれが異なる `kind_tag` を返すこと (DO ID 名前空間衝突回避)。
    #[test]
    fn each_kind_has_unique_tag() {
        let kinds = [
            RateLimitKind::LobbyLoginPerIp,
            RateLimitKind::LobbyLoginPerHandle,
            RateLimitKind::LobbyChallengePerIp,
            RateLimitKind::LobbyChallengePerInviter,
            RateLimitKind::RoomCreatePerIp,
            RateLimitKind::WsRoomUpgradePerIp,
        ];
        let mut tags: Vec<&'static str> = kinds.iter().map(|k| k.kind_tag()).collect();
        tags.sort_unstable();
        let unique: Vec<&'static str> = {
            let mut v = tags.clone();
            v.dedup();
            v
        };
        assert_eq!(tags, unique, "kind_tag() must be unique per kind");
    }

    /// 設計 doc §5.2 で要求される pure logic: 多 IP 並列で互いに干渉しない。
    /// 同 capacity の bucket 2 個を交互に consume しても他方の残量が減らない
    /// (これは関数レベル test では「2 つの bucket struct を別々に持つ」ことで
    /// 直接示せる)。
    #[test]
    fn two_buckets_are_independent() {
        let mut b1 = TokenBucketState::full(2, 0);
        let mut b2 = TokenBucketState::full(2, 0);
        b1.try_consume(2, 0);
        b1.try_consume(2, 0);
        // b1 deny
        assert!(!b1.try_consume(2, 0).allowed);
        // b2 は手付かずなので 2 回 allow できる
        assert!(b2.try_consume(2, 0).allowed);
        assert!(b2.try_consume(2, 0).allowed);
    }

    /// `Duration` 型との互換: `RateLimitDecision::retry_after_sec` を `Duration`
    /// に直して Worker 側 sleep に渡しやすいことを確認する (回帰防止)。
    #[test]
    fn retry_after_sec_converts_to_duration() {
        let d = RateLimitDecision::deny(15);
        assert_eq!(Duration::from_secs(d.retry_after_sec), Duration::from_secs(15));
    }
}
