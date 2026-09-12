//! viewer 配信用 R2 prefix の補完 / orphan 掃除を担う cron ジョブ群。
//!
//! 以下 3 つの best-effort ジョブを実装する:
//!
//! - [`run_games_index_backfill`]: `kifu-by-id/` を 256 件単位で巡回し、meta 本文から
//!   `games-index/<inv>-<id>.json` key を再生成して上書き put する (派生 index 補完)。
//! - [`run_games_search_backfill`]: `games-index/` を巡回し、D1 検索 index を補完する。
//! - [`run_live_orphan_sweep`]: `live-games-index/` を pagination loop で list
//!   し、対応する `kifu-by-id/<id>.meta.json` (= 終局済 primary 判定キー、設計
//!   v3 §3) が存在する live entry を delete する (orphan 掃除)。https://github.com/SH11235/rshogi/issues/629 で
//!   1 page → 複数 page (共有 deadline 内) に拡張した。
//!
//! index backfill は D1 に R2 cursor を永続化し、1 cron につき 1 page ずつ進む。
//! prefix 末尾まで到達したら cursor をリセットし、将来追加された entry も拾えるよう
//! 先頭から巡回を再開する。3 ジョブは cron 30s 制限の安全側
//! (`SCHEDULED_WORK_DEADLINE_MS`) を共有する。
//! admin invoke endpoint や 1 万件超の bulk 並列化はスコープ外
//! (設計 v3 §10)。
//!
//! 進捗ログは [`structured_log!`](crate::structured_log) で JSON 化して
//! Cloudflare Workers の Logs / tail へ流す (Cloudflare Workers Logs Phase A)。
//! いかなる失敗 (R2/D1 binding 解決失敗 / state 読み書き失敗 / list 失敗 /
//! get 失敗 / put 失敗 / parse 失敗) も `Err` を返さずログのみ残して
//! `Ok` で抜ける契約。
//! `scheduled` handler が次回 cron 起動を妨げないようにするため、伝播禁止。
//!
//! # ホスト target でのテスト境界
//!
//! `worker` クレートは wasm32 限定なので、IO 本体 ([`run_games_index_backfill`]
//! / [`run_live_orphan_sweep`]) は `cfg(target_arch = "wasm32")` でゲートする。
//! 純粋ロジック (Stats 構造体 / `MetaForIndexKey` deserialize) はホスト target
//! でも参照可能で、`cargo test` でこれらの形状契約を検証する。

use serde::Deserialize;

/// `kifu-by-id/` prefix。`<id>.csa` と `<id>.meta.json` が同居するため、
/// `.meta.json` で suffix 判定する側でこの prefix を再利用する。
pub(crate) const KIFU_BY_ID_PREFIX: &str = "kifu-by-id/";

/// `kifu-by-id/<id>.meta.json` の suffix。list 結果から meta だけを抽出する。
pub(crate) const META_SUFFIX: &str = ".meta.json";

/// 1 cron run あたりの list page size (= R2 list の最大値)。
///
/// 1000 を超える backfill 対象が常時残る運用に到達したら admin invoke endpoint
/// 経由で複数ページ一気に処理する案 (設計 v2 §5 (a)) を別 issue で検討する。
pub(crate) const PAGE_SIZE: u32 = 1000;

/// D1 state に保存する完了マーカー。R2 cursor と衝突しない予約値。
const GAMES_SEARCH_BACKFILL_COMPLETE: &str = "__COMPLETE__";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchBackfillStart {
    Scan,
    HealRecent,
}

impl SearchBackfillStart {
    /// ログ用の識別文字列。運用時に Scan(通常バックフィル) と
    /// HealRecent(完了後の自己修復) を区別できるようにする。
    fn as_log_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::HealRecent => "heal_recent",
        }
    }
}

impl SearchBackfillStart {
    fn initial_cursor(self, saved_cursor: Option<String>) -> Option<String> {
        match self {
            Self::Scan => saved_cursor,
            Self::HealRecent => None,
        }
    }

    fn list_limit(self) -> u32 {
        match self {
            Self::Scan => PAGE_SIZE,
            Self::HealRecent => GAMES_SEARCH_HEAL_LIMIT,
        }
    }

    fn max_pages(self) -> Option<u32> {
        match self {
            Self::Scan => None,
            Self::HealRecent => Some(1),
        }
    }
}

fn search_backfill_start(cursor: Option<&str>) -> SearchBackfillStart {
    if cursor == Some(GAMES_SEARCH_BACKFILL_COMPLETE) {
        SearchBackfillStart::HealRecent
    } else {
        SearchBackfillStart::Scan
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchBackfillItemOutcome {
    PermanentSkip,
    R2GetError,
    R2BodyMissing,
    R2BodyReadError,
    D1UpsertError,
    DeadlineExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchBackfillItemControl {
    Continue,
    RetryPage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchBackfillStateOperation<'a> {
    RetryPage,
    CursorMissing,
    UpdateCursor(&'a str),
    MarkComplete,
}

#[derive(Debug, Default)]
struct SearchBackfillPageState {
    retry_page: bool,
}

impl SearchBackfillPageState {
    fn record(&mut self, outcome: SearchBackfillItemOutcome) -> SearchBackfillItemControl {
        match outcome {
            SearchBackfillItemOutcome::PermanentSkip => SearchBackfillItemControl::Continue,
            SearchBackfillItemOutcome::R2GetError
            | SearchBackfillItemOutcome::R2BodyMissing
            | SearchBackfillItemOutcome::R2BodyReadError
            | SearchBackfillItemOutcome::D1UpsertError
            | SearchBackfillItemOutcome::DeadlineExceeded => {
                self.retry_page = true;
                SearchBackfillItemControl::RetryPage
            }
        }
    }

    fn finish<'a>(
        self,
        truncated: bool,
        next_cursor: Option<&'a str>,
    ) -> SearchBackfillStateOperation<'a> {
        if self.retry_page {
            SearchBackfillStateOperation::RetryPage
        } else if truncated {
            next_cursor.map_or(
                SearchBackfillStateOperation::CursorMissing,
                SearchBackfillStateOperation::UpdateCursor,
            )
        } else {
            SearchBackfillStateOperation::MarkComplete
        }
    }
}

/// games-search backfill と orphan sweep が共有する 1 cron 内の処理期限。
///
/// Cloudflare Workers の cron 起動は wall-clock 30s 制限を持つため、安全側
/// マージン (5s) を引いた 25s を cron 発火時刻から共有する。search backfill が
/// 長引いた場合、後続 sweep の残り時間はその分だけ減り、0 にもなりうる
/// (:15/:30/:45 は sweep 単独で従来同等の実質 25s を使えるため、:00 で
/// sweep が飢餓しても最大 15 分で全予算に回復する)。
pub(crate) const SCHEDULED_WORK_DEADLINE_MS: u64 = 25_000;

/// 完了後の自己修復で毎 cron に再 upsert する最新 games-index 件数。
///
/// 「1 時間あたりの終局数が本値を超えない」という前提の下でのみ、D1 upsert
/// 失敗からの eventual recovery を保証する (この前提を超える局数が同一時間内に
/// 終局すると、押し出された古いエントリの再訪は保証されない)。R2 が正本のため
/// 実害は検索結果からの一時的な欠落のみで、`games_search_backfill_state` の
/// `r2_cursor` を手動でリセットすれば全走査を再開できる。
const GAMES_SEARCH_HEAL_LIMIT: u32 = 100;

fn shared_budget_remaining_ms(started_at_ms: u64, now_ms: u64) -> u64 {
    SCHEDULED_WORK_DEADLINE_MS.saturating_sub(now_ms.saturating_sub(started_at_ms))
}

fn shared_deadline_reached(started_at_ms: u64, now_ms: u64) -> bool {
    shared_budget_remaining_ms(started_at_ms, now_ms) == 0
}

/// `run_live_orphan_sweep` の安全側 page 上限 (https://github.com/SH11235/rshogi/issues/629)。共有 deadline
/// を超えなくても、cursor が永遠に truncated を返し続ける異常時に無限 loop を
/// 避けるための break 条件。100 page = 100,000 件で十分な余白。
pub(crate) const SWEEP_MAX_PAGES: u32 = 100;

/// live entry の hard-TTL backstop (72 時間)。
///
/// 通常の sweep は「primary meta (`kifu-by-id/<id>.meta.json`) が存在する live
/// entry」だけを消し、meta 未配置の entry は「進行中」または「終局時 meta PUT
/// 失敗」の両義があるため保守的に残す (設計 v3 §3)。しかし
/// `force_finalize_unrecoverable` 経路は R2 export / meta を書かずに終局させる
/// ため、inline の live-index delete が retry を尽くして失敗すると、meta が
/// 永遠に現れず通常 sweep でも回収できない幽霊 entry が残る (#853 系)。
///
/// これを回収するため、`started_at_ms` (= `play_started_at_ms`) が本 TTL より
/// 古い live entry は meta の有無に関わらず削除する。72 時間はどんな持ち時間の
/// 正規対局よりも遥かに長く、正当に進行中の対局が誤って消える現実的リスクは
/// 無い。仮に消しても実害は「`/api/v1/games/live` 一覧から隠れる」だけで、
/// 対局そのもの (DO / WS / 終局処理) には一切影響しない。誤削除の検知のため、
/// hard-TTL 削除は必ず error レベルの structured_log を残す。
pub(crate) const LIVE_ENTRY_HARD_TTL_MS: u64 = 72 * 60 * 60 * 1000;

/// `run_games_index_backfill` の進捗統計。テスト容易性のため値型で返す。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BackfillStats {
    /// list で観測した meta オブジェクトの総数。
    pub listed: u64,
    /// `games-index/` への put が成功した件数 (上書き含む)。
    pub put: u64,
    /// key 生成失敗 / parse 失敗 / 必須フィールド欠如等で put を skip した件数。
    pub skipped: u64,
    /// deadline により、list で観測した meta を全件処理しなかった場合に `true`。
    /// cursor は進めず、未処理ページ全体を次回 cron で再試行する。
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GamesIndexBackfillItemOutcome {
    Put,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GamesIndexBackfillStateOperation<'a> {
    RetryPage,
    CursorMissing,
    UpdateCursor(&'a str),
    ResetCursor,
}

fn games_index_backfill_state_operation(
    page_fully_processed: bool,
    truncated: bool,
    next_cursor: Option<&str>,
) -> GamesIndexBackfillStateOperation<'_> {
    if !page_fully_processed {
        GamesIndexBackfillStateOperation::RetryPage
    } else if truncated {
        next_cursor.map_or(
            GamesIndexBackfillStateOperation::CursorMissing,
            GamesIndexBackfillStateOperation::UpdateCursor,
        )
    } else {
        GamesIndexBackfillStateOperation::ResetCursor
    }
}

impl BackfillStats {
    fn record_games_index_outcome(&mut self, outcome: GamesIndexBackfillItemOutcome) {
        match outcome {
            GamesIndexBackfillItemOutcome::Put => {
                self.put = self.put.saturating_add(1);
            }
            GamesIndexBackfillItemOutcome::Skipped => {
                self.skipped = self.skipped.saturating_add(1);
            }
        }
    }

    fn mark_games_index_truncated_if_unprocessed(&mut self) {
        self.truncated = self.put.saturating_add(self.skipped) < self.listed;
    }
}

/// Workers の同時オープン接続数 6 本を前提に、1 バッチを cron 契約の deadline
/// margin 5 秒以内に収めつつ、バッチ間で後続ジョブへ共有予算を譲る。
const GAMES_INDEX_BACKFILL_BATCH_SIZE: usize = 16;

/// get + put の subrequest 予算を後続ジョブにも残すため、R2 list と 1 invocation
/// の処理対象を同じ 256 件に制限する。
const GAMES_INDEX_BACKFILL_MAX_ITEMS: u32 = 256;

fn games_index_backfill_batch_should_start(started_at_ms: u64, now_ms: u64) -> bool {
    !shared_deadline_reached(started_at_ms, now_ms)
}

/// `run_live_orphan_sweep` の進捗統計。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepStats {
    /// list で観測した live-games-index entry の総数。
    pub listed: u64,
    /// `kifu-by-id/<id>.meta.json` が存在する (= 終局済) ため delete した件数。
    pub deleted: u64,
    /// meta 不在だが `LIVE_ENTRY_HARD_TTL_MS` を超過したため delete した件数
    /// (hard-TTL backstop)。`deleted` とは別カウントにして、幽霊 entry の回収が
    /// 発生した事実をログ / metric から検知できるようにする。恒常的に非 0 なら
    /// finalize 経路の live-index delete が慢性的に失敗している疑い。
    pub hard_ttl_deleted: u64,
    /// `kifu-by-id/<id>.meta.json` が不在かつ hard-TTL 内で保持した live entry 件数。
    /// meta は終局時に書かれるため、**通常の進行中対局も終局までは meta 不在**で
    /// ここに一時的に計上される。したがって本値は「ゾンビ(終局済みだが kifu 未書き込み)」
    /// そのものではなく「meta 不在の live entry 数(進行中 + export 失敗)」の gauge。
    /// R2 の可視信号だけでは進行中と export 失敗を正の判定で区別できない(meta 自体が
    /// 終局信号)ため、真のゾンビ候補は下の `oldest_live_without_meta_age_ms` が通常の
    /// 対局時間を大きく超えるエントリとして掃除判断に使う。本文を読めず状態不明だった
    /// entry (`read_live_entry_fields` が `None`) は計上しないため、値は下振れしうる。
    pub live_without_meta_within_ttl: u64,
    /// `live_without_meta_within_ttl` に計上した entry の最大 age。対象が無い場合は 0。
    /// 通常の対局時間を大きく超える値は「終局済みだが export 失敗した真のゾンビ」の
    /// 主要シグナル。
    pub oldest_live_without_meta_age_ms: u64,
    /// 走査した R2 list page 数 (https://github.com/SH11235/rshogi/issues/629)。pagination loop 化に伴って導入。
    /// 1 cron で複数 page を処理した状況をログから確認するための運用 metric。
    pub pages: u32,
    /// 共有 deadline 経過で打ち切った場合に `true`。`true` の cron が
    /// 連続したら page size または cron 頻度の見直しが必要 (https://github.com/SH11235/rshogi/issues/629)。
    pub deadline_reached: bool,
    /// `SWEEP_MAX_PAGES` に到達して打ち切った場合に `true`。summary の件数が
    /// 部分走査であることを明示するために記録する。
    pub max_pages_reached: bool,
    /// bucket binding 取得失敗 / R2 list 失敗 / truncated なのに cursor 欠落、
    /// といった異常で走査を打ち切った場合に `true`。deadline / max_pages と併せて
    /// summary の件数が完全走査でないことを示す。監視側が「meta 不在 0 件」を
    /// 完全走査の 0 と誤認しないための gate。
    pub aborted: bool,
}

impl SweepStats {
    pub(crate) fn record_live_without_meta(&mut self, age_ms: u64) {
        self.live_without_meta_within_ttl = self.live_without_meta_within_ttl.saturating_add(1);
        self.oldest_live_without_meta_age_ms = self.oldest_live_without_meta_age_ms.max(age_ms);
    }
}

/// `kifu-by-id/<id>.meta.json` の本文を deserialize する最小 view。
///
/// `GamesIndexEntry` は `&'a str` 借用ベースなので Deserialize できない。
/// backfill 経路では `ended_at_ms` と `game_id` だけが key 再構築に必要なので、
/// 必要 field だけを持つ owned 型を別に置く (将来 meta 形式が拡張されても、
/// ここで参照する 2 field の wire 名が安定している限り影響を受けない)。
#[derive(Debug, Deserialize)]
pub(crate) struct MetaForIndexKey {
    pub game_id: String,
    pub ended_at_ms: u64,
}

/// `live-games-index/<inv>-<id>.json` の本文から orphan sweep 判定に必要な field
/// のみ取り出す最小 view。`game_id` (meta key 構築 + ログ用) と `started_at_ms`
/// (hard-TTL backstop 用) だけを持ち、それ以外の形式 (clock 等) には依存しない。
#[derive(Debug, Deserialize)]
pub(crate) struct LiveEntryFields {
    pub game_id: String,
    pub started_at_ms: u64,
}

/// hard-TTL backstop の削除判定 (純粋関数、host からテスト可能)。
///
/// `age_ms` = sweep 実行時刻 − live entry の `started_at_ms`。これが
/// [`LIVE_ENTRY_HARD_TTL_MS`] 以上なら、primary meta が不在でも幽霊 entry と
/// みなして削除対象にする (境界は「以上」= inclusive)。
pub(crate) fn live_entry_hard_ttl_expired(age_ms: u64) -> bool {
    age_ms >= LIVE_ENTRY_HARD_TTL_MS
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{
        BackfillStats, GAMES_INDEX_BACKFILL_BATCH_SIZE, GAMES_INDEX_BACKFILL_MAX_ITEMS,
        GAMES_SEARCH_BACKFILL_COMPLETE, GamesIndexBackfillItemOutcome,
        GamesIndexBackfillStateOperation, KIFU_BY_ID_PREFIX, LIVE_ENTRY_HARD_TTL_MS,
        LiveEntryFields, META_SUFFIX, MetaForIndexKey, PAGE_SIZE, SWEEP_MAX_PAGES,
        SearchBackfillItemOutcome, SearchBackfillPageState, SearchBackfillStateOperation,
        SweepStats, games_index_backfill_batch_should_start, games_index_backfill_state_operation,
        live_entry_hard_ttl_expired, search_backfill_start, shared_deadline_reached,
    };
    use worker::{Date, Env, Result};

    use crate::config::ConfigKeys;
    use crate::games_index::KEY_PREFIX as GAMES_INDEX_PREFIX;
    use crate::games_index::games_index_key;
    use crate::games_search_index::{OwnedGamesIndexEntry, upsert_owned};
    use crate::live_games_index::LIVE_KEY_PREFIX;
    use crate::x1_paths::kifu_by_id_meta_key;

    /// `kifu-by-id/` を 256 件で 1 ページ list し、各 meta 本文から
    /// `games-index/<inv>-<id>.json` を再生成して上書き put する。
    ///
    /// 上書き put は冪等 (R2 strongly consistent 上書き、設計 v2 §2)。head
    /// による存在チェックは行わない。D1 に cursor を保存して次回 cron で続行し、
    /// prefix 末尾まで走査したら cursor をリセットして先頭から巡回し直す。
    ///
    /// 各失敗 (binding 失敗 / list 失敗 / get 失敗 / parse 失敗 / key 生成失敗
    /// / put 失敗) は logfmt で記録し `Err` を伝播しない。集計結果のみを返す。
    pub async fn run_games_index_backfill(
        env: &Env,
        scheduled_started_at_ms: u64,
    ) -> Result<BackfillStats> {
        let started_at_ms = Date::now().as_millis();
        let mut stats = BackfillStats::default();

        let bucket = match env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
            Ok(b) => b,
            Err(e) => {
                crate::structured_log!(
                    event: "games_index_backfill_bucket_failed",
                    component: "backfill",
                    err: format!("{e:?}"),
                );
                return Ok(stats);
            }
        };

        let db = match env.d1(ConfigKeys::GAMES_SEARCH_DB_BINDING) {
            Ok(db) => db,
            Err(e) => {
                crate::structured_log!(event: "games_index_backfill_d1_failed", component: "backfill", err: format!("{e:?}"));
                return Ok(stats);
            }
        };
        let saved_cursor = match db
            .prepare("SELECT r2_cursor FROM games_index_backfill_state WHERE singleton = 1")
            .first::<String>(Some("r2_cursor"))
            .await
        {
            Ok(cursor) => cursor,
            Err(e) => {
                crate::structured_log!(event: "games_index_backfill_state_read_failed", component: "backfill", err: format!("{e:?}"));
                None
            }
        };

        let mut list =
            bucket.list().prefix(KIFU_BY_ID_PREFIX).limit(GAMES_INDEX_BACKFILL_MAX_ITEMS);
        if let Some(cursor) = saved_cursor.as_deref() {
            list = list.cursor(cursor);
        }
        let page = match list.execute().await {
            Ok(p) => p,
            Err(e) => {
                crate::structured_log!(
                    event: "games_index_backfill_list_failed",
                    component: "backfill",
                    err: format!("{e:?}"),
                );
                return Ok(stats);
            }
        };

        // `kifu-by-id/<id>.csa` も同 prefix に出るため、`.meta.json` 拡張子で
        // 絞り込む。`.csa` は無視 (legacy fallback は本 issue Non-goals)。
        let keys: Vec<_> = page
            .objects()
            .into_iter()
            .filter_map(|obj| {
                let key = obj.key();
                key.ends_with(META_SUFFIX).then_some(key)
            })
            .collect();
        stats.listed = keys.len() as u64;
        for keys in keys.chunks(GAMES_INDEX_BACKFILL_BATCH_SIZE) {
            if !games_index_backfill_batch_should_start(
                scheduled_started_at_ms,
                Date::now().as_millis(),
            ) {
                break;
            }

            let futures = keys.iter().map(|key| {
                let bucket = &bucket;
                async move {
                    let fetched = match bucket.get(key).execute().await {
                        Ok(o) => o,
                        Err(e) => {
                            crate::structured_log!(
                                event: "games_index_backfill_get_failed",
                                component: "backfill",
                                key: key,
                                err: format!("{e:?}"),
                            );
                            return GamesIndexBackfillItemOutcome::Skipped;
                        }
                    };
                    let Some(fetched) = fetched else {
                        // list と get の間に削除されたケース。skip 集計。
                        return GamesIndexBackfillItemOutcome::Skipped;
                    };
                    let Some(body) = fetched.body() else {
                        return GamesIndexBackfillItemOutcome::Skipped;
                    };
                    let bytes = match body.bytes().await {
                        Ok(b) => b,
                        Err(e) => {
                            crate::structured_log!(
                                event: "games_index_backfill_read_failed",
                                component: "backfill",
                                key: key,
                                err: format!("{e:?}"),
                            );
                            return GamesIndexBackfillItemOutcome::Skipped;
                        }
                    };
                    let meta: MetaForIndexKey = match serde_json::from_slice(&bytes) {
                        Ok(v) => v,
                        Err(e) => {
                            crate::structured_log!(
                                event: "games_index_backfill_parse_failed",
                                component: "backfill",
                                key: key,
                                err: format!("{e:?}"),
                            );
                            return GamesIndexBackfillItemOutcome::Skipped;
                        }
                    };

                    let index_key = match games_index_key(meta.ended_at_ms, &meta.game_id) {
                        Ok(k) => k,
                        Err(e) => {
                            crate::structured_log!(
                                event: "games_index_backfill_key_failed",
                                component: "backfill",
                                game_id: meta.game_id,
                                err: format!("{e:?}"),
                            );
                            return GamesIndexBackfillItemOutcome::Skipped;
                        }
                    };

                    // body は meta の wire そのまま。`GamesIndexEntry` の wire と等価
                    // (両方とも export_kifu_to_r2 で同一 JSON を put している)。
                    if let Err(e) = bucket.put(&index_key, bytes).execute().await {
                        crate::structured_log!(
                            event: "games_index_backfill_put_failed",
                            component: "backfill",
                            game_id: meta.game_id,
                            index_key: index_key,
                            err: format!("{e:?}"),
                        );
                        return GamesIndexBackfillItemOutcome::Skipped;
                    }
                    GamesIndexBackfillItemOutcome::Put
                }
            });

            for outcome in futures_util::future::join_all(futures).await {
                stats.record_games_index_outcome(outcome);
            }
        }

        stats.mark_games_index_truncated_if_unprocessed();

        let next_cursor = page.cursor();
        let state_operation = games_index_backfill_state_operation(
            !stats.truncated,
            page.truncated(),
            next_cursor.as_deref(),
        );
        let state_statement = match state_operation {
            GamesIndexBackfillStateOperation::RetryPage => None,
            GamesIndexBackfillStateOperation::CursorMissing => {
                crate::structured_log!(event: "games_index_backfill_cursor_missing", component: "backfill");
                None
            }
            GamesIndexBackfillStateOperation::UpdateCursor(cursor) => {
                let bind = [worker::wasm_bindgen::JsValue::from_str(cursor)];
                match db
                    .prepare("INSERT INTO games_index_backfill_state (singleton, r2_cursor) VALUES (1, ?) ON CONFLICT(singleton) DO UPDATE SET r2_cursor=excluded.r2_cursor")
                    .bind(&bind)
                {
                    Ok(statement) => Some(statement),
                    Err(e) => {
                        crate::structured_log!(event: "games_index_backfill_state_prepare_failed", component: "backfill", err: format!("{e:?}"));
                        None
                    }
                }
            }
            GamesIndexBackfillStateOperation::ResetCursor => {
                Some(db.prepare("DELETE FROM games_index_backfill_state WHERE singleton = 1"))
            }
        };
        if let Some(statement) = state_statement {
            if let Err(e) = statement.run().await {
                crate::structured_log!(event: "games_index_backfill_state_write_failed", component: "backfill", err: format!("{e:?}"));
            }
        }

        let elapsed_ms = Date::now().as_millis().saturating_sub(started_at_ms);
        crate::structured_log!(
            event: "games_index_backfill_progress",
            component: "backfill",
            listed: stats.listed,
            put: stats.put,
            skipped: stats.skipped,
            truncated: stats.truncated,
            elapsed_ms: elapsed_ms,
        );
        Ok(stats)
    }

    /// R2 `games-index/` をページングし、D1 検索 index を冪等 upsert する。
    /// cron 発火時刻から sweep と共有する 25 秒以内だけ処理する。
    pub async fn run_games_search_backfill(
        env: &Env,
        scheduled_started_at_ms: u64,
    ) -> Result<BackfillStats> {
        let mut stats = BackfillStats::default();
        let bucket = match env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
            Ok(bucket) => bucket,
            Err(e) => {
                crate::structured_log!(event: "games_search_backfill_bucket_failed", component: "backfill", err: format!("{e:?}"));
                return Ok(stats);
            }
        };
        let db = match env.d1(ConfigKeys::GAMES_SEARCH_DB_BINDING) {
            Ok(db) => db,
            Err(e) => {
                crate::structured_log!(event: "games_search_backfill_d1_failed", component: "backfill", err: format!("{e:?}"));
                return Ok(stats);
            }
        };
        let saved_cursor = match db
            .prepare("SELECT r2_cursor FROM games_search_backfill_state WHERE singleton = 1")
            .first::<String>(Some("r2_cursor"))
            .await
        {
            Ok(cursor) => cursor,
            Err(e) => {
                crate::structured_log!(event: "games_search_backfill_state_read_failed", component: "backfill", err: format!("{e:?}"));
                return Ok(stats);
            }
        };
        let start = search_backfill_start(saved_cursor.as_deref());
        let mut cursor = start.initial_cursor(saved_cursor);
        let mut pages = 0_u32;
        loop {
            if shared_deadline_reached(scheduled_started_at_ms, Date::now().as_millis()) {
                break;
            }
            // 完了後の自己修復は newest-first の先頭 100 件を cursor なしで 1 page
            // だけ再 upsert する固定コスト処理。独立した deadline は設けず、開始前と
            // 各 object 後の共有 deadline 判定に含める。
            let mut builder = bucket.list().prefix(GAMES_INDEX_PREFIX).limit(start.list_limit());
            if let Some(value) = cursor.as_deref() {
                builder = builder.cursor(value);
            }
            let page = match builder.execute().await {
                Ok(page) => page,
                Err(e) => {
                    crate::structured_log!(event: "games_search_backfill_list_failed", component: "backfill", err: format!("{e:?}"));
                    break;
                }
            };
            pages = pages.saturating_add(1);
            let mut page_state = SearchBackfillPageState::default();
            for object in page.objects() {
                if shared_deadline_reached(scheduled_started_at_ms, Date::now().as_millis()) {
                    page_state.record(SearchBackfillItemOutcome::DeadlineExceeded);
                    break;
                }
                let key = object.key();
                stats.listed = stats.listed.saturating_add(1);
                let fetched = match bucket.get(&key).execute().await {
                    Ok(Some(object)) => object,
                    Ok(None) => {
                        crate::structured_log!(event: "games_search_backfill_get_missing", component: "backfill", key: key);
                        stats.skipped = stats.skipped.saturating_add(1);
                        continue;
                    }
                    Err(e) => {
                        crate::structured_log!(event: "games_search_backfill_get_failed", component: "backfill", key: key, err: format!("{e:?}"));
                        page_state.record(SearchBackfillItemOutcome::R2GetError);
                        stats.skipped = stats.skipped.saturating_add(1);
                        break;
                    }
                };
                let Some(body) = fetched.body() else {
                    crate::structured_log!(event: "games_search_backfill_body_missing", component: "backfill", key: key);
                    page_state.record(SearchBackfillItemOutcome::R2BodyMissing);
                    stats.skipped = stats.skipped.saturating_add(1);
                    break;
                };
                let bytes = match body.bytes().await {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        crate::structured_log!(event: "games_search_backfill_read_failed", component: "backfill", key: key, err: format!("{e:?}"));
                        page_state.record(SearchBackfillItemOutcome::R2BodyReadError);
                        stats.skipped = stats.skipped.saturating_add(1);
                        break;
                    }
                };
                let entry = match serde_json::from_slice::<OwnedGamesIndexEntry>(&bytes) {
                    Ok(entry) => entry,
                    Err(e) => {
                        // JSON 破損は恒久エラーとしてこの object だけを skip する。
                        crate::structured_log!(event: "games_search_backfill_parse_failed", component: "backfill", key: key, err: format!("{e:?}"));
                        stats.skipped = stats.skipped.saturating_add(1);
                        page_state.record(SearchBackfillItemOutcome::PermanentSkip);
                        continue;
                    }
                };
                match upsert_owned(env, &entry).await {
                    Ok(()) => stats.put = stats.put.saturating_add(1),
                    Err(e) => {
                        crate::structured_log!(event: "games_search_backfill_upsert_failed", component: "backfill", game_id: entry.game_id, err: format!("{e:?}"));
                        stats.skipped = stats.skipped.saturating_add(1);
                        page_state.record(SearchBackfillItemOutcome::D1UpsertError);
                        break;
                    }
                }
                if shared_deadline_reached(scheduled_started_at_ms, Date::now().as_millis()) {
                    page_state.record(SearchBackfillItemOutcome::DeadlineExceeded);
                    break;
                }
            }
            if start.max_pages().is_some_and(|max_pages| pages >= max_pages) {
                break;
            }
            let next_cursor = page.cursor();
            let operation = page_state.finish(page.truncated(), next_cursor.as_deref());
            let state_value = match operation {
                SearchBackfillStateOperation::RetryPage => break,
                SearchBackfillStateOperation::CursorMissing => {
                    crate::structured_log!(event: "games_search_backfill_cursor_missing", component: "backfill");
                    break;
                }
                SearchBackfillStateOperation::UpdateCursor(value) => value,
                SearchBackfillStateOperation::MarkComplete => GAMES_SEARCH_BACKFILL_COMPLETE,
            };
            let bind = [worker::wasm_bindgen::JsValue::from_str(state_value)];
            let state_statement = match db
                .prepare("INSERT INTO games_search_backfill_state (singleton, r2_cursor) VALUES (1, ?) ON CONFLICT(singleton) DO UPDATE SET r2_cursor=excluded.r2_cursor")
                .bind(&bind)
            {
                Ok(statement) => statement,
                Err(e) => {
                    crate::structured_log!(event: "games_search_backfill_state_prepare_failed", component: "backfill", err: format!("{e:?}"));
                    break;
                }
            };
            if let Err(e) = state_statement.run().await {
                crate::structured_log!(event: "games_search_backfill_state_write_failed", component: "backfill", err: format!("{e:?}"));
                break;
            }
            if operation == SearchBackfillStateOperation::MarkComplete {
                break;
            }
            cursor = Some(state_value.to_owned());
        }
        crate::structured_log!(event: "games_search_backfill_progress", component: "backfill", mode: start.as_log_str(), listed: stats.listed, put: stats.put, skipped: stats.skipped);
        Ok(stats)
    }

    /// `live-games-index/<inv>-<id>.json` の各 entry について、対応する終局済
    /// meta (`kifu-by-id/<id>.meta.json`) が存在する live entry を delete する。
    ///
    /// 設計 v3 §3 に従い、判定キーは `kifu-by-id/<id>.csa` ではなく `.meta.json`。
    /// CSA 本体 put は失敗していても meta が書かれていれば終局確定 +
    /// finalize_if_ended 経路を通った証拠になる。逆もまた然りで、両方失敗した
    /// orphan は本 sweep の **通常経路** では消さない (= eventual に live 一覧に
    /// 残るが、次回 cron で finalize 経路の副作用で meta が put された後に消える、
    /// または手動オペレーションで対処)。
    ///
    /// ただし meta を書かない `force_finalize_unrecoverable` 経路の inline
    /// live-index delete が retry を尽くして失敗すると、meta が永遠に現れず通常
    /// 経路では回収できない幽霊 entry が残る (#853 系)。これに対する最終防衛線と
    /// して **hard-TTL backstop** を持つ: live entry の `started_at_ms` が
    /// [`LIVE_ENTRY_HARD_TTL_MS`] (72h) より古ければ meta の有無に関わらず削除し、
    /// error レベルログ + `SweepStats.hard_ttl_deleted` を計上する。正当に進行中の
    /// 対局が誤って消えても実害は「live 一覧から隠れる」だけで対局自体には影響
    /// しない ([`live_entry_hard_ttl_expired`] / `docs/csa-server/viewer_access_control.md` §7.4)。
    ///
    /// https://github.com/SH11235/rshogi/issues/629 で pagination loop 化した。R2 list の `truncated` が `true`
    /// の間は cursor を辿って次 page を処理し、以下のいずれかの条件で打ち切る:
    ///
    /// 1. `truncated() == false` (全件処理完了)
    /// 2. cron 発火からの経過時間 ≥ `SCHEDULED_WORK_DEADLINE_MS` (search backfill
    ///    と共有する安全
    ///    側打ち切り。object loop 内でも判定して 1 page 完走を待たない)
    /// 3. `pages >= SWEEP_MAX_PAGES` (異常時の無限 loop ガード)
    ///
    /// 打ち切った残りは次回 cron で先頭から再走査する (cursor は永続化しない =
    /// live key prefix の昇順 lexicographic に依存した再走査)。
    ///
    /// 各失敗は logfmt で記録し `Err` を伝播しない。
    pub async fn run_live_orphan_sweep(
        env: &Env,
        scheduled_started_at_ms: u64,
    ) -> Result<SweepStats> {
        let started_at_ms = scheduled_started_at_ms;
        let mut stats = SweepStats::default();

        let bucket = match env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
            Ok(b) => b,
            Err(e) => {
                crate::structured_log!(
                    event: "live_orphan_sweep_bucket_failed",
                    component: "backfill",
                    err: format!("{e:?}"),
                );
                stats.aborted = true;
                log_live_orphan_sweep_summary(&stats, started_at_ms);
                return Ok(stats);
            }
        };

        let mut cursor: Option<String> = None;
        'outer: loop {
            // 各 page 取得前に deadline をチェックする
            // (https://github.com/SH11235/rshogi/issues/654)。各 object 処理後の
            // deadline チェックは loop 内にあるが、次反復先頭の `builder.execute`
            // 前に確認しておかないと 30s cron 制限の境界で 1 page 余分に R2 list
            // を発行してしまうケースが残る。1 page 取得 (R2 list) は約 50-200ms
            // 必要なので、deadline ギリギリで再 list するより安全側で break する。
            if shared_deadline_reached(started_at_ms, Date::now().as_millis()) {
                stats.deadline_reached = true;
                break 'outer;
            }
            let mut builder = bucket.list().prefix(LIVE_KEY_PREFIX).limit(PAGE_SIZE);
            if let Some(c) = cursor.as_ref() {
                builder = builder.cursor(c);
            }
            let page = match builder.execute().await {
                Ok(p) => p,
                Err(e) => {
                    crate::structured_log!(
                        event: "live_orphan_sweep_list_failed",
                        component: "backfill",
                        pages: stats.pages,
                        err: format!("{e:?}"),
                    );
                    stats.aborted = true;
                    break;
                }
            };
            stats.pages = stats.pages.saturating_add(1);

            for obj in page.objects() {
                let live_key = obj.key();
                stats.listed = stats.listed.saturating_add(1);

                // live entry 本文から game_id / started_at_ms を取り出す。key 文字列
                // パースより本文 field を信頼するほうが、key 形式の将来変更に
                // 対して頑健。
                let LiveEntryFields {
                    game_id,
                    started_at_ms: entry_started_at_ms,
                } = match read_live_entry_fields(&bucket, &live_key).await {
                    Some(f) => f,
                    None => {
                        if shared_deadline_reached(started_at_ms, Date::now().as_millis()) {
                            stats.deadline_reached = true;
                            break 'outer;
                        }
                        continue;
                    }
                };

                // primary meta が存在 = 終局済 → live は orphan として delete 対象。
                let meta_key = kifu_by_id_meta_key(&game_id);
                let head_result = match bucket.head(&meta_key).await {
                    Ok(o) => o,
                    Err(e) => {
                        crate::structured_log!(
                            event: "live_orphan_sweep_head_failed",
                            component: "backfill",
                            game_id: game_id,
                            meta_key: meta_key,
                            err: format!("{e:?}"),
                        );
                        if shared_deadline_reached(started_at_ms, Date::now().as_millis()) {
                            stats.deadline_reached = true;
                            break 'outer;
                        }
                        continue;
                    }
                };

                // hard-TTL backstop: meta 不在でも `started_at_ms` が
                // `LIVE_ENTRY_HARD_TTL_MS` より古ければ削除する。TTL 判定の「now」
                // 基準には sweep 開始時刻 (`started_at_ms` 変数、最大 25s の stale)
                // を流用する (72h に対して無視できる)。`force_finalize_unrecoverable`
                // 経路の live-index delete が retry を尽くして失敗した幽霊 entry を
                // 回収するための最終防衛線 (#853 系)。
                let age_ms = started_at_ms.saturating_sub(entry_started_at_ms);
                let hard_ttl_expired = live_entry_hard_ttl_expired(age_ms);
                if head_result.is_none() && !hard_ttl_expired {
                    // meta が無い & TTL 内 = まだ進行中 (or 終局時 meta put 失敗)。
                    // 前者は正常状態、後者は本 sweep の対象外 (設計 v3 §3 の意図的
                    // な保守)。
                    stats.record_live_without_meta(age_ms);
                    if shared_deadline_reached(started_at_ms, Date::now().as_millis()) {
                        stats.deadline_reached = true;
                        break 'outer;
                    }
                    continue;
                }

                if let Err(e) = bucket.delete(&live_key).await {
                    crate::structured_log!(
                        event: "live_orphan_sweep_delete_failed",
                        component: "backfill",
                        game_id: game_id,
                        live_key: live_key,
                        err: format!("{e:?}"),
                    );
                    if shared_deadline_reached(started_at_ms, Date::now().as_millis()) {
                        stats.deadline_reached = true;
                        break 'outer;
                    }
                    continue;
                }
                if head_result.is_none() {
                    // hard-TTL backstop 経路で幽霊 entry を回収した。誤削除の検知
                    // と finalize 経路の慢性失敗の検知のため、必ず error レベルで
                    // 残す (通常 orphan delete とは別カウント)。
                    stats.hard_ttl_deleted = stats.hard_ttl_deleted.saturating_add(1);
                    crate::structured_log!(
                        event: "live_orphan_sweep_hard_ttl_deleted",
                        component: "backfill",
                        level: "error",
                        game_id: game_id,
                        live_key: live_key,
                        started_at_ms: entry_started_at_ms,
                        age_ms: age_ms,
                        ttl_ms: LIVE_ENTRY_HARD_TTL_MS,
                    );
                } else {
                    stats.deleted = stats.deleted.saturating_add(1);
                }

                if shared_deadline_reached(started_at_ms, Date::now().as_millis()) {
                    stats.deadline_reached = true;
                    break 'outer;
                }
            }

            if !page.truncated() {
                break;
            }
            if stats.pages >= SWEEP_MAX_PAGES {
                stats.max_pages_reached = true;
                crate::structured_log!(
                    event: "live_orphan_sweep_max_pages_reached",
                    component: "backfill",
                    pages: stats.pages,
                );
                break;
            }
            cursor = page.cursor();
            if cursor.is_none() {
                // ここは line 488 の `!truncated` break を通過済み = truncated == true。
                // にもかかわらず cursor が None は R2 仕様上通常起こらない異常なので、
                // 部分走査として安全側に break し aborted を立てる。
                stats.aborted = true;
                crate::structured_log!(
                    event: "live_orphan_sweep_cursor_missing",
                    component: "backfill",
                    pages: stats.pages,
                );
                break;
            }
        }

        let elapsed_ms = Date::now().as_millis().saturating_sub(started_at_ms);
        crate::structured_log!(
            event: "live_orphan_sweep_progress",
            component: "backfill",
            listed: stats.listed,
            deleted: stats.deleted,
            hard_ttl_deleted: stats.hard_ttl_deleted,
            pages: stats.pages,
            deadline_reached: stats.deadline_reached,
            elapsed_ms: elapsed_ms,
        );
        log_live_orphan_sweep_summary(&stats, started_at_ms);
        Ok(stats)
    }

    fn log_live_orphan_sweep_summary(stats: &SweepStats, started_at_ms: u64) {
        let elapsed_ms = Date::now().as_millis().saturating_sub(started_at_ms);
        crate::structured_log!(
            event: "live_orphan_sweep_summary",
            component: "backfill",
            level: "info",
            total_live_entries_scanned: stats.listed,
            finished_deleted: stats.deleted,
            hard_ttl_deleted: stats.hard_ttl_deleted,
            live_without_meta_within_ttl: stats.live_without_meta_within_ttl,
            oldest_live_without_meta_age_ms: stats.oldest_live_without_meta_age_ms,
            pages_scanned: stats.pages,
            deadline_reached: stats.deadline_reached,
            max_pages_reached: stats.max_pages_reached,
            aborted: stats.aborted,
            partial_scan: stats.deadline_reached || stats.max_pages_reached || stats.aborted,
            elapsed_ms: elapsed_ms,
        );
    }

    /// `live-games-index/<key>` の本文を読んで orphan sweep 判定に必要な field
    /// (`game_id` / `started_at_ms`) を返す。
    ///
    /// 失敗はすべて構造化ログで記録した上で `None` を返し、呼び出し側で entry
    /// を skip させる (sweep 全体を停止しない)。
    async fn read_live_entry_fields(bucket: &worker::Bucket, key: &str) -> Option<LiveEntryFields> {
        let fetched = match bucket.get(key).execute().await {
            Ok(o) => o,
            Err(e) => {
                crate::structured_log!(
                    event: "live_orphan_sweep_get_failed",
                    component: "backfill",
                    key: key,
                    err: format!("{e:?}"),
                );
                return None;
            }
        };
        let fetched = fetched?;
        let body = fetched.body()?;
        let bytes = match body.bytes().await {
            Ok(b) => b,
            Err(e) => {
                crate::structured_log!(
                    event: "live_orphan_sweep_read_failed",
                    component: "backfill",
                    key: key,
                    err: format!("{e:?}"),
                );
                return None;
            }
        };
        match serde_json::from_slice::<LiveEntryFields>(&bytes) {
            Ok(v) => Some(v),
            Err(e) => {
                crate::structured_log!(
                    event: "live_orphan_sweep_parse_failed",
                    component: "backfill",
                    key: key,
                    err: format!("{e:?}"),
                );
                None
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use imp::{run_games_index_backfill, run_games_search_backfill, run_live_orphan_sweep};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x1_paths::kifu_by_id_meta_key;

    #[test]
    fn meta_for_index_key_deserializes_subset_of_games_index_entry() {
        // `GamesIndexEntry` の wire と互換であることを確認する。
        let json = r#"{
            "game_id": "lobby-cross-fischer-1777391025209",
            "started_at_ms": 1777391025209,
            "ended_at_ms": 1777392877244,
            "black_handle": "alice",
            "white_handle": "bob",
            "result_kind": "WIN_BLACK",
            "end_reason": "RESIGN",
            "moves_count": 142,
            "clock": {"kind": "fischer", "total_sec": 300, "increment_sec": 5},
            "source": "kifu"
        }"#;
        let parsed: MetaForIndexKey = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.game_id, "lobby-cross-fischer-1777391025209");
        assert_eq!(parsed.ended_at_ms, 1_777_392_877_244);
    }

    #[test]
    fn meta_for_index_key_rejects_missing_required_fields() {
        // `game_id` 欠落は parse error → backfill 経路で skip。
        let json = r#"{"ended_at_ms": 1}"#;
        assert!(serde_json::from_str::<MetaForIndexKey>(json).is_err());

        // `ended_at_ms` 欠落も同様。
        let json = r#"{"game_id": "g1"}"#;
        assert!(serde_json::from_str::<MetaForIndexKey>(json).is_err());
    }

    #[test]
    fn live_entry_fields_deserializes_from_live_entry_wire() {
        // `LiveGamesIndexEntry` の wire (`live_games_index::tests::live_entry_serializes_with_expected_fields`
        // と整合) から `game_id` / `started_at_ms` を抽出できる。
        let json = r#"{
            "game_id": "g1",
            "started_at_ms": 1777391025209,
            "black_handle": "alice",
            "white_handle": "bob",
            "clock": {"kind": "fischer", "total_sec": 300, "increment_sec": 5},
            "source": "kifu"
        }"#;
        let parsed: LiveEntryFields = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.game_id, "g1");
        assert_eq!(parsed.started_at_ms, 1_777_391_025_209);
    }

    #[test]
    fn live_entry_fields_rejects_missing_started_at_ms() {
        // `started_at_ms` 欠落は parse error → sweep 経路で skip (hard-TTL 判定
        // 材料が無いため保守的に触らない)。
        let json = r#"{"game_id": "g1"}"#;
        assert!(serde_json::from_str::<LiveEntryFields>(json).is_err());
    }

    #[test]
    fn live_entry_hard_ttl_expired_boundary() {
        // TTL 未満は保持、ちょうど TTL 以上で削除対象 (inclusive 境界)。
        assert!(!live_entry_hard_ttl_expired(0));
        assert!(!live_entry_hard_ttl_expired(LIVE_ENTRY_HARD_TTL_MS - 1));
        assert!(live_entry_hard_ttl_expired(LIVE_ENTRY_HARD_TTL_MS));
        assert!(live_entry_hard_ttl_expired(LIVE_ENTRY_HARD_TTL_MS + 1));
    }

    #[test]
    fn games_index_backfill_best_effort_records_each_item_independently() {
        let mut stats = BackfillStats {
            listed: 3,
            ..BackfillStats::default()
        };

        for outcome in [
            GamesIndexBackfillItemOutcome::Put,
            GamesIndexBackfillItemOutcome::Skipped,
            GamesIndexBackfillItemOutcome::Put,
        ] {
            stats.record_games_index_outcome(outcome);
        }

        assert_eq!(
            stats,
            BackfillStats {
                listed: 3,
                put: 2,
                skipped: 1,
                truncated: false,
            }
        );
    }

    #[test]
    fn games_index_backfill_does_not_start_batch_after_shared_deadline() {
        let started_at_ms = 1_000;
        assert!(games_index_backfill_batch_should_start(
            started_at_ms,
            started_at_ms + SCHEDULED_WORK_DEADLINE_MS - 1
        ));
        assert!(!games_index_backfill_batch_should_start(
            started_at_ms,
            started_at_ms + SCHEDULED_WORK_DEADLINE_MS
        ));
    }

    #[test]
    fn games_index_backfill_limits_are_fixed_for_workers_budgets() {
        assert_eq!(GAMES_INDEX_BACKFILL_BATCH_SIZE, 16);
        assert_eq!(GAMES_INDEX_BACKFILL_MAX_ITEMS, 256);
        const _: () = assert!(GAMES_INDEX_BACKFILL_MAX_ITEMS < PAGE_SIZE);
        assert_eq!(GAMES_INDEX_BACKFILL_MAX_ITEMS as usize % GAMES_INDEX_BACKFILL_BATCH_SIZE, 0);
    }

    #[test]
    fn games_index_backfill_stats_marks_unprocessed_items_as_truncated() {
        let mut stats = BackfillStats {
            listed: 300,
            put: 255,
            skipped: 1,
            ..BackfillStats::default()
        };

        stats.mark_games_index_truncated_if_unprocessed();

        assert!(stats.truncated);
    }

    #[test]
    fn games_index_backfill_cursor_rotates_after_last_page() {
        assert_eq!(
            games_index_backfill_state_operation(true, true, Some("next")),
            GamesIndexBackfillStateOperation::UpdateCursor("next")
        );
        assert_eq!(
            games_index_backfill_state_operation(true, false, None),
            GamesIndexBackfillStateOperation::ResetCursor
        );
        assert_eq!(
            games_index_backfill_state_operation(false, true, Some("next")),
            GamesIndexBackfillStateOperation::RetryPage
        );
        assert_eq!(
            games_index_backfill_state_operation(true, true, None),
            GamesIndexBackfillStateOperation::CursorMissing
        );
    }

    #[test]
    fn search_backfill_deadline_selects_retry_page_and_keeps_cursor() {
        let mut state = SearchBackfillPageState::default();
        assert_eq!(
            state.record(SearchBackfillItemOutcome::DeadlineExceeded),
            SearchBackfillItemControl::RetryPage
        );
        assert_eq!(state.finish(true, Some("next")), SearchBackfillStateOperation::RetryPage);
    }

    #[test]
    fn search_backfill_temporary_io_failures_keep_cursor() {
        for failure in [
            SearchBackfillItemOutcome::R2GetError,
            SearchBackfillItemOutcome::R2BodyMissing,
            SearchBackfillItemOutcome::R2BodyReadError,
            SearchBackfillItemOutcome::D1UpsertError,
        ] {
            let mut state = SearchBackfillPageState::default();
            assert_eq!(state.record(failure), SearchBackfillItemControl::RetryPage, "{failure:?}");
            assert_eq!(
                state.finish(true, Some("next")),
                SearchBackfillStateOperation::RetryPage,
                "{failure:?}"
            );
        }
    }

    #[test]
    fn search_backfill_json_parse_failure_advances_cursor() {
        let mut state = SearchBackfillPageState::default();
        assert_eq!(
            state.record(SearchBackfillItemOutcome::PermanentSkip),
            SearchBackfillItemControl::Continue
        );
        assert_eq!(
            state.finish(true, Some("next")),
            SearchBackfillStateOperation::UpdateCursor("next")
        );
    }

    #[test]
    fn search_backfill_completion_marker_runs_one_bounded_healing_page() {
        let start = search_backfill_start(Some(GAMES_SEARCH_BACKFILL_COMPLETE));
        assert_eq!(start, SearchBackfillStart::HealRecent);
        assert_eq!(start.list_limit(), 100);
        assert_eq!(start.max_pages(), Some(1));
        assert_eq!(start.initial_cursor(Some(GAMES_SEARCH_BACKFILL_COMPLETE.to_owned())), None);
        assert_eq!(start.as_log_str(), "heal_recent");
        assert_eq!(search_backfill_start(None).as_log_str(), "scan");
    }

    #[test]
    fn shared_budget_consumed_by_search_leaves_nothing_for_sweep() {
        let cron_started_at_ms = 1_000;
        let search_finished_at_ms = cron_started_at_ms + SCHEDULED_WORK_DEADLINE_MS;
        assert_eq!(shared_budget_remaining_ms(cron_started_at_ms, search_finished_at_ms), 0);
        assert!(shared_deadline_reached(cron_started_at_ms, search_finished_at_ms));
    }

    #[test]
    fn sweep_stats_records_live_without_meta_summary() {
        let mut stats = SweepStats::default();

        stats.record_live_without_meta(1_000);
        stats.record_live_without_meta(500);
        stats.record_live_without_meta(2_500);

        assert_eq!(stats.live_without_meta_within_ttl, 3);
        assert_eq!(stats.oldest_live_without_meta_age_ms, 2_500);
    }

    #[test]
    fn sweep_deadline_is_safely_below_workers_30s_limit() {
        // Cloudflare Workers cron の wall-clock 制限は 30s。search backfill と
        // sweep の共有 deadline は安全側マージン (≥ 5s) を確保していないと、検知後の
        // pagination break + 後続ログ出力中に 30s 制限を踏む恐れがある。
        const _: () = assert!(
            SCHEDULED_WORK_DEADLINE_MS + 5_000 <= 30_000,
            "shared scheduled work deadline must leave a 5s margin under the 30s cron limit",
        );
    }

    #[test]
    fn sweep_max_pages_caps_runaway_pagination() {
        // 共有 deadline を超えなくても、cursor が壊れて truncated を
        // 返し続けるような異常時に無限 loop を避けるための gate。1 page =
        // 1000 件で 100 page = 100,000 件は live-games-index の現実的な
        // 上限を大きく超えている。
        const _: () = assert!(SWEEP_MAX_PAGES >= 1, "SWEEP_MAX_PAGES must allow at least 1 page");
    }

    #[test]
    fn meta_suffix_matches_kifu_by_id_meta_key_layout() {
        // `kifu_by_id_meta_key` 生成キーの拡張子と本モジュールの list filter で
        // 使う suffix が必ず揃っていること (片方だけ変わると backfill が空振り)。
        let key = kifu_by_id_meta_key("g1");
        assert!(key.ends_with(META_SUFFIX), "key={key} suffix={META_SUFFIX}");
    }

    #[test]
    fn kifu_by_id_meta_key_starts_with_backfill_prefix() {
        // backfill list 走査の prefix と meta key の先頭は揃っていること。
        let key = kifu_by_id_meta_key("g1");
        assert!(key.starts_with(KIFU_BY_ID_PREFIX), "key={key} prefix={KIFU_BY_ID_PREFIX}");
    }

    #[test]
    fn page_size_does_not_exceed_r2_list_limit() {
        // R2 list の上限 = 1000 (Cloudflare 仕様)。本値を勝手に上げると runtime
        // 失敗するため、定数の不変条件として固定。const block で生成時に検査
        // させる (clippy::assertions_on_constants 回避)。
        const _: () = assert!(PAGE_SIZE <= 1000, "PAGE_SIZE must not exceed R2 list limit");
    }
}
