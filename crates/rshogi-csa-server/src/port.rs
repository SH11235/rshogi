//! Port trait 群。I/O 具体実装はフロントエンド側のアダプタに委譲する。
//!
//! Rust 1.75+ の native `async fn in trait` で非同期メソッドを定義する。
//! `Send` 境界を付けず、Cloudflare Workers（wasm32 シングルスレッド）と
//! tokio（マルチスレッド）の双方で使えるように配慮している。
//! マルチスレッド実行環境で各アダプタインスタンスを共有する場合は、
//! 上位でロック等の排他制御を行う想定。

use std::time::Duration;

use crate::error::{StorageError, TransportError};
use crate::types::{CsaLine, GameId, GameName, IpKey, PlayerName, RoomId, StorageKey};

/// 配信対象タグ。`Broadcaster` 実装で宛先を絞るための識別子。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BroadcastTag {
    /// 対局者向け。
    Player,
    /// 観戦者向け。
    Spectator,
    /// 運営向け（管理コマンドの結果通知など）。
    Admin,
}

/// 1 クライアント接続の行 I/O 抽象。
///
/// TCP（`tokio::net::TcpStream`）と WebSocket（Durable Object）のどちらも、
/// この trait を実装したアダプタを通してコアに接続される。
pub trait ClientTransport {
    /// 1 行受信。`timeout` を経過すれば [`TransportError::Timeout`]、EOF は [`TransportError::Closed`]。
    fn recv_line(
        &mut self,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<CsaLine, TransportError>>;

    /// 1 行送信（末尾改行はアダプタ側で付与する）。
    fn send_line(
        &mut self,
        line: &CsaLine,
    ) -> impl std::future::Future<Output = Result<(), TransportError>>;

    /// 接続を能動的に閉じる。
    fn close(&mut self) -> impl std::future::Future<Output = Result<(), TransportError>>;

    /// クライアント識別子（デバッグログ／レート制限用）。
    fn peer_id(&self) -> IpKey;
}

/// ルーム配信抽象。対局者・観戦者への送信を一括で扱う。
pub trait Broadcaster {
    /// 指定ルームの全接続に 1 行配信する。
    fn broadcast_room(
        &self,
        room_id: &RoomId,
        line: &CsaLine,
    ) -> impl std::future::Future<Output = Result<(), TransportError>>;

    /// 指定ルーム内のタグ付き接続のみに配信する。
    fn broadcast_tag(
        &self,
        room_id: &RoomId,
        tag: BroadcastTag,
        line: &CsaLine,
    ) -> impl std::future::Future<Output = Result<(), TransportError>>;
}

/// 00LIST 追記用のサマリエントリ。
#[derive(Debug, Clone)]
pub struct GameSummaryEntry {
    /// 対局 ID。
    pub game_id: GameId,
    /// 先手プレイヤ名。
    pub sente: PlayerName,
    /// 後手プレイヤ名。
    pub gote: PlayerName,
    /// 対局開始 UTC 時刻（ISO 8601）。
    pub start_time: String,
    /// 対局終了 UTC 時刻（ISO 8601）。
    pub end_time: String,
    /// 終局コード（`#RESIGN` 等）。
    pub result_code: String,
}

/// 棋譜・00LIST の永続化抽象。
pub trait KifuStorage {
    /// CSA V2 棋譜を保存し、実際の保存キーを返す。
    fn save(
        &self,
        game_id: &GameId,
        csa_v2_text: &str,
    ) -> impl std::future::Future<Output = Result<StorageKey, StorageError>>;

    /// 既存の CSA V2 棋譜を読み出す。
    ///
    /// `%%FORK` のように過去棋譜から任意手数の局面を再構築する経路で使う。
    /// 未保存の `game_id` は `Ok(None)` を返す。
    fn load(
        &self,
        game_id: &GameId,
    ) -> impl std::future::Future<Output = Result<Option<String>, StorageError>>;

    /// 00LIST に 1 行追記する。
    fn append_summary(
        &self,
        entry: &GameSummaryEntry,
    ) -> impl std::future::Future<Output = Result<(), StorageError>>;
}

/// プレイヤレート 1 件分の記録。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerRateRecord {
    /// プレイヤ名。
    pub name: PlayerName,
    /// レーティング。
    pub rate: i32,
    /// 勝数。
    pub wins: u32,
    /// 負数。
    pub losses: u32,
    /// 最終対局 ID。
    pub last_game_id: Option<GameId>,
    /// 最終更新時刻（ISO 8601）。
    pub last_modified: String,
}

/// プレイヤレート永続化抽象（players.yaml 互換 / KV namespace 等を差し替え可能）。
pub trait RateStorage {
    /// プレイヤ名に対応するレコードをロードする。
    fn load(
        &self,
        name: &PlayerName,
    ) -> impl std::future::Future<Output = Result<Option<PlayerRateRecord>, StorageError>>;

    /// レコードを保存する（既存があれば置換）。
    fn save(
        &self,
        record: &PlayerRateRecord,
    ) -> impl std::future::Future<Output = Result<(), StorageError>>;

    /// 全件列挙。
    fn list_all(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<PlayerRateRecord>, StorageError>>;

    /// 終局時のレート関連フィールドを更新する（atomic な read-modify-write）。
    ///
    /// レート値そのもの（`rate`）は外部バッチ（Ruby `mk_rate` 等）が更新する責務
    /// なので本関数では触れず、勝敗 / `last_game_id` / `last_modified` のみを
    /// 更新する。`winner` が `Some(name)` なら該当者の `wins` を +1、それ以外の
    /// 既知プレイヤの `losses` を +1 する。`winner` が `None`（千日手・最大手数・
    /// 切断で勝者不確定 `Abnormal { winner: None }`）なら `wins`/`losses` は据置で
    /// `last_*` のみ更新する。
    ///
    /// 既定実装は `load` → 変更 → `save` を逐次実行するため、複数対局が同時に
    /// 同一プレイヤのレコードを書き換えると最後の `save` で勝敗増分が失われる
    /// レースを抱える。アトミック性が必要な実装（[`crate::FileKifuStorage`] 系の
    /// ファイルベース）は本関数を override して内部 lock 配下で読み書きする。
    fn record_game_outcome(
        &self,
        black: &PlayerName,
        white: &PlayerName,
        winner: Option<&PlayerName>,
        game_id: &GameId,
        now_iso: &str,
    ) -> impl std::future::Future<Output = Result<(), StorageError>> {
        async move {
            for name in [black, white] {
                let mut rec = match self.load(name).await? {
                    Some(r) => r,
                    None => continue,
                };
                match winner {
                    Some(w) if w == name => rec.wins = rec.wins.saturating_add(1),
                    Some(_) => rec.losses = rec.losses.saturating_add(1),
                    None => {}
                }
                rec.last_game_id = Some(game_id.clone());
                rec.last_modified = now_iso.to_owned();
                self.save(&rec).await?;
            }
            Ok(())
        }
    }
}

/// ブイ（途中局面テンプレート）の永続化抽象。
pub trait BuoyStorage {
    /// ブイを登録／更新する。
    fn set(
        &self,
        game_name: &GameName,
        moves: Vec<crate::types::CsaMoveToken>,
        remaining: u32,
    ) -> impl std::future::Future<Output = Result<(), StorageError>>;

    /// ブイを削除する。
    fn delete(
        &self,
        game_name: &GameName,
    ) -> impl std::future::Future<Output = Result<(), StorageError>>;

    /// 残り対局数を取得する。
    fn count(
        &self,
        game_name: &GameName,
    ) -> impl std::future::Future<Output = Result<Option<u32>, StorageError>>;
}

/// レート制限の判定結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateDecision {
    /// 許容。
    Allow,
    /// 拒否（`retry_after_sec` 秒後に再試行可能）。
    Deny {
        /// 次に再試行可能になるまでの秒数。
        retry_after_sec: u64,
    },
}
