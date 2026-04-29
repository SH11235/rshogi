//! viewer 配信用 R2 prefix の補完 / orphan 掃除を担う cron ジョブ群。
//!
//! Issue #551 設計 v3 に従い、以下 2 つの best-effort ジョブを実装する:
//!
//! - [`run_games_index_backfill`]: `kifu-by-id/<id>.meta.json` を 1 ページ
//!   (1000 件) 単位で list し、各 meta 本文から `games-index/<inv>-<id>.json`
//!   key を再生成して上書き put する (派生 index 補完)。
//! - [`run_live_orphan_sweep`]: `live-games-index/` を 1 ページ単位で list し、
//!   対応する `kifu-by-id/<id>.meta.json` (= 終局済 primary 判定キー、設計 v3
//!   §3) が存在する live entry を delete する (orphan 掃除)。
//!
//! いずれも 1 page (1000 件) のみ処理し、cursor の持ち越しは行わない (= 次回
//! cron で続行する eventual semantics)。本実装範囲では admin invoke endpoint
//! や 1 万件超の bulk 並列化は Non-goals (設計 v3 §10)。
//!
//! 進捗ログは logfmt 構造化 (`event=…_progress listed=… elapsed_ms=…`) で
//! `console_log!` に流す。Cloudflare Workers の Logs / tail で grep 可能。
//! いかなる失敗 (R2 binding 解決失敗 / list 失敗 / get 失敗 / put 失敗 /
//! parse 失敗) も `Err` を返さず ログのみ残して `Ok` で抜ける契約。
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
/// `.meta.json` で suffix 判定する側で本 prefix を再利用する。
pub(crate) const KIFU_BY_ID_PREFIX: &str = "kifu-by-id/";

/// `kifu-by-id/<id>.meta.json` の suffix。list 結果から meta だけを抽出する。
pub(crate) const META_SUFFIX: &str = ".meta.json";

/// 1 cron run あたりの list page size (= R2 list の最大値)。
///
/// 1000 を超える backfill 対象が常時残る運用に到達したら admin invoke endpoint
/// 経由で複数ページ一気に処理する案 (設計 v2 §5 (a)) を別 issue で検討する。
pub(crate) const PAGE_SIZE: u32 = 1000;

/// `run_games_index_backfill` の進捗統計。テスト容易性のため値型で返す。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BackfillStats {
    /// list で観測した meta オブジェクトの総数。
    pub listed: u64,
    /// `games-index/` への put が成功した件数 (上書き含む)。
    pub put: u64,
    /// key 生成失敗 / parse 失敗 / 必須フィールド欠如等で put を skip した件数。
    pub skipped: u64,
}

/// `run_live_orphan_sweep` の進捗統計。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepStats {
    /// list で観測した live-games-index entry の総数。
    pub listed: u64,
    /// `kifu-by-id/<id>.meta.json` が存在する (= 終局済) ため delete した件数。
    pub deleted: u64,
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

/// `live-games-index/<inv>-<id>.json` の本文から `game_id` field のみ取り出す
/// 最小 view。orphan sweep 判定に必要なのは `game_id` のみなので、それ以外の
/// 形式 (clock 等) には依存しない。
#[derive(Debug, Deserialize)]
pub(crate) struct LiveEntryGameId {
    pub game_id: String,
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{
        BackfillStats, KIFU_BY_ID_PREFIX, LiveEntryGameId, META_SUFFIX, MetaForIndexKey, PAGE_SIZE,
        SweepStats,
    };
    use worker::{Date, Env, Result, console_log};

    use crate::config::ConfigKeys;
    use crate::games_index::games_index_key;
    use crate::live_games_index::LIVE_KEY_PREFIX;
    use crate::x1_paths::kifu_by_id_meta_key;

    /// `kifu-by-id/*.meta.json` を 1 ページ list し、各 meta 本文から
    /// `games-index/<inv>-<id>.json` を再生成して上書き put する。
    ///
    /// 上書き put は冪等 (R2 strongly consistent 上書き、設計 v2 §2)。head
    /// による存在チェックは行わない。cursor の持ち越しは「次回 cron で続行」
    /// する eventual semantics (設計 v2 §5)。
    ///
    /// 各失敗 (binding 失敗 / list 失敗 / get 失敗 / parse 失敗 / key 生成失敗
    /// / put 失敗) は logfmt で記録し `Err` を伝播しない。集計結果のみを返す。
    pub async fn run_games_index_backfill(env: &Env) -> Result<BackfillStats> {
        let started_at_ms = Date::now().as_millis();
        let mut stats = BackfillStats::default();

        let bucket = match env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
            Ok(b) => b,
            Err(e) => {
                console_log!("[backfill] event=games_index_backfill_bucket_failed err={:?}", e);
                return Ok(stats);
            }
        };

        let page = match bucket.list().prefix(KIFU_BY_ID_PREFIX).limit(PAGE_SIZE).execute().await {
            Ok(p) => p,
            Err(e) => {
                console_log!("[backfill] event=games_index_backfill_list_failed err={:?}", e);
                return Ok(stats);
            }
        };

        for obj in page.objects() {
            let key = obj.key();
            // `kifu-by-id/<id>.csa` も同 prefix に出るため、`.meta.json` 拡張子で
            // 絞り込む。`.csa` は無視 (legacy fallback は本 issue Non-goals)。
            if !key.ends_with(META_SUFFIX) {
                continue;
            }
            stats.listed = stats.listed.saturating_add(1);

            let fetched = match bucket.get(&key).execute().await {
                Ok(o) => o,
                Err(e) => {
                    console_log!(
                        "[backfill] event=games_index_backfill_get_failed key={} err={:?}",
                        key,
                        e,
                    );
                    stats.skipped = stats.skipped.saturating_add(1);
                    continue;
                }
            };
            let Some(fetched) = fetched else {
                // list と get の間に削除されたケース。skip 集計。
                stats.skipped = stats.skipped.saturating_add(1);
                continue;
            };
            let Some(body) = fetched.body() else {
                stats.skipped = stats.skipped.saturating_add(1);
                continue;
            };
            let bytes = match body.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    console_log!(
                        "[backfill] event=games_index_backfill_read_failed key={} err={:?}",
                        key,
                        e,
                    );
                    stats.skipped = stats.skipped.saturating_add(1);
                    continue;
                }
            };
            let meta: MetaForIndexKey = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    console_log!(
                        "[backfill] event=games_index_backfill_parse_failed key={} err={:?}",
                        key,
                        e,
                    );
                    stats.skipped = stats.skipped.saturating_add(1);
                    continue;
                }
            };

            let index_key = match games_index_key(meta.ended_at_ms, &meta.game_id) {
                Ok(k) => k,
                Err(e) => {
                    console_log!(
                        "[backfill] event=games_index_backfill_key_failed game_id={} err={:?}",
                        meta.game_id,
                        e,
                    );
                    stats.skipped = stats.skipped.saturating_add(1);
                    continue;
                }
            };

            // body は meta の wire そのまま。`GamesIndexEntry` の wire と等価
            // (両方とも export_kifu_to_r2 で同一 JSON を put している)。
            if let Err(e) = bucket.put(&index_key, bytes).execute().await {
                console_log!(
                    "[backfill] event=games_index_backfill_put_failed game_id={} index_key={} err={:?}",
                    meta.game_id,
                    index_key,
                    e,
                );
                stats.skipped = stats.skipped.saturating_add(1);
                continue;
            }
            stats.put = stats.put.saturating_add(1);
        }

        let elapsed_ms = Date::now().as_millis().saturating_sub(started_at_ms);
        console_log!(
            "[backfill] event=games_index_backfill_progress listed={} put={} skipped={} elapsed_ms={}",
            stats.listed,
            stats.put,
            stats.skipped,
            elapsed_ms,
        );
        Ok(stats)
    }

    /// `live-games-index/<inv>-<id>.json` の各 entry について、対応する終局済
    /// meta (`kifu-by-id/<id>.meta.json`) が存在する live entry を delete する。
    ///
    /// 設計 v3 §3 に従い、判定キーは `kifu-by-id/<id>.csa` ではなく `.meta.json`。
    /// CSA 本体 put は失敗していても meta が書かれていれば終局確定 +
    /// finalize_if_ended 経路を通った証拠になる。逆もまた然りで、両方失敗した
    /// orphan は本 sweep では消さない (= eventual に live 一覧に残るが、次回
    /// cron で finalize 経路の副作用で meta が put された後に消える、または
    /// 手動オペレーションで対処)。
    ///
    /// 各失敗は logfmt で記録し `Err` を伝播しない。
    pub async fn run_live_orphan_sweep(env: &Env) -> Result<SweepStats> {
        let started_at_ms = Date::now().as_millis();
        let mut stats = SweepStats::default();

        let bucket = match env.bucket(ConfigKeys::KIFU_BUCKET_BINDING) {
            Ok(b) => b,
            Err(e) => {
                console_log!("[backfill] event=live_orphan_sweep_bucket_failed err={:?}", e);
                return Ok(stats);
            }
        };

        let page = match bucket.list().prefix(LIVE_KEY_PREFIX).limit(PAGE_SIZE).execute().await {
            Ok(p) => p,
            Err(e) => {
                console_log!("[backfill] event=live_orphan_sweep_list_failed err={:?}", e);
                return Ok(stats);
            }
        };

        for obj in page.objects() {
            let live_key = obj.key();
            stats.listed = stats.listed.saturating_add(1);

            // live entry 本文から game_id を取り出す。key 文字列パースより本文
            // の `game_id` field を信頼するほうが、key 形式の将来変更に対して
            // 頑健。
            let game_id = match read_live_entry_game_id(&bucket, &live_key).await {
                Some(id) => id,
                None => continue,
            };

            // primary meta が存在 = 終局済 → live は orphan として delete 対象。
            let meta_key = kifu_by_id_meta_key(&game_id);
            let head_result = match bucket.head(&meta_key).await {
                Ok(o) => o,
                Err(e) => {
                    console_log!(
                        "[backfill] event=live_orphan_sweep_head_failed game_id={} meta_key={} err={:?}",
                        game_id,
                        meta_key,
                        e,
                    );
                    continue;
                }
            };
            if head_result.is_none() {
                // meta が無い = まだ進行中 (or 終局時 meta put 失敗)。前者は
                // 正常状態、後者は本 sweep の対象外 (設計 v3 §3 の意図的な保守)。
                continue;
            }

            if let Err(e) = bucket.delete(&live_key).await {
                console_log!(
                    "[backfill] event=live_orphan_sweep_delete_failed game_id={} live_key={} err={:?}",
                    game_id,
                    live_key,
                    e,
                );
                continue;
            }
            stats.deleted = stats.deleted.saturating_add(1);
        }

        let elapsed_ms = Date::now().as_millis().saturating_sub(started_at_ms);
        console_log!(
            "[backfill] event=live_orphan_sweep_progress listed={} deleted={} elapsed_ms={}",
            stats.listed,
            stats.deleted,
            elapsed_ms,
        );
        Ok(stats)
    }

    /// `live-games-index/<key>` の本文を読んで `game_id` field を返す。
    ///
    /// 失敗はすべて logfmt で記録した上で `None` を返し、呼び出し側で entry
    /// を skip させる (sweep 全体を停止しない)。
    async fn read_live_entry_game_id(bucket: &worker::Bucket, key: &str) -> Option<String> {
        let fetched = match bucket.get(key).execute().await {
            Ok(o) => o,
            Err(e) => {
                console_log!(
                    "[backfill] event=live_orphan_sweep_get_failed key={} err={:?}",
                    key,
                    e,
                );
                return None;
            }
        };
        let fetched = fetched?;
        let body = fetched.body()?;
        let bytes = match body.bytes().await {
            Ok(b) => b,
            Err(e) => {
                console_log!(
                    "[backfill] event=live_orphan_sweep_read_failed key={} err={:?}",
                    key,
                    e,
                );
                return None;
            }
        };
        match serde_json::from_slice::<LiveEntryGameId>(&bytes) {
            Ok(v) => Some(v.game_id),
            Err(e) => {
                console_log!(
                    "[backfill] event=live_orphan_sweep_parse_failed key={} err={:?}",
                    key,
                    e,
                );
                None
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use imp::{run_games_index_backfill, run_live_orphan_sweep};

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
    fn live_entry_game_id_deserializes_from_live_entry_wire() {
        // `LiveGamesIndexEntry` の wire (`live_games_index::tests::live_entry_serializes_with_expected_fields`
        // と整合) から `game_id` のみ抽出できる。
        let json = r#"{
            "game_id": "g1",
            "started_at_ms": 1777391025209,
            "black_handle": "alice",
            "white_handle": "bob",
            "clock": {"kind": "fischer", "total_sec": 300, "increment_sec": 5},
            "source": "kifu"
        }"#;
        let parsed: LiveEntryGameId = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.game_id, "g1");
    }

    #[test]
    fn backfill_stats_default_is_zero() {
        let stats = BackfillStats::default();
        assert_eq!(
            stats,
            BackfillStats {
                listed: 0,
                put: 0,
                skipped: 0
            }
        );
    }

    #[test]
    fn sweep_stats_default_is_zero() {
        let stats = SweepStats::default();
        assert_eq!(
            stats,
            SweepStats {
                listed: 0,
                deleted: 0
            }
        );
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
