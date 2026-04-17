//! 同一プロセス内の観戦者接続を束ねる in-memory [`Broadcaster`] 実装。
//!
//! Phase 1 では観戦経路は空実装でも動くが、[`rshogi_csa_server::game::run_loop::run_room`]
//! が `BroadcastTag::Spectator` を必ず 1 回以上呼ぶため、副作用が起きても安全なスタブを
//! 用意しておく。
//!
//! 設計上、各ルームの観戦者集合は `Mutex<HashMap<RoomId, Vec<Subscriber>>>` で保持する。
//! 1 subscriber あたり `tokio::sync::mpsc::UnboundedSender<CsaLine>` を持たせ、
//! 実際の送信は受信側タスクが担う（I/O をロック内で行わないようにするため）。

use std::collections::HashMap;
use std::sync::Arc;

use rshogi_csa_server::TransportError;
use rshogi_csa_server::port::{BroadcastTag, Broadcaster};
use rshogi_csa_server::types::{CsaLine, RoomId};
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;

/// 1 人分の観戦者ハンドル。
#[derive(Clone)]
pub struct Subscriber {
    /// 受信側タスクへ 1 行を送る送信口。エラー（受信タスク停止）は `retain` で掃除される。
    tx: UnboundedSender<CsaLine>,
}

impl Subscriber {
    /// 与えられた送信口で Subscriber を作る。
    pub fn new(tx: UnboundedSender<CsaLine>) -> Self {
        Self { tx }
    }
}

/// プロセスローカルの `Broadcaster`。
///
/// Phase 1 MVP では 1 プロセスに 1 インスタンスだけ作り、`Arc` で共有する想定。
/// 複数プロセス間の配信は Phase 2（Workers）以降のスコープ。
#[derive(Default, Clone)]
pub struct InMemoryBroadcaster {
    inner: Arc<Mutex<HashMap<RoomId, Vec<Subscriber>>>>,
}

impl InMemoryBroadcaster {
    /// 空のブロードキャスタを作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// ルームに観戦者を登録する（Phase 3 の `%%MONITOR2ON` から呼ばれる想定）。
    /// Phase 1 では呼び出し経路を用意しておくだけで、本体コードからは使わない。
    pub async fn subscribe(&self, room_id: RoomId, subscriber: Subscriber) {
        let mut guard = self.inner.lock().await;
        guard.entry(room_id).or_default().push(subscriber);
    }

    /// ルームに紐づく観戦者集合を丸ごと削除する。対局終了時などに呼ぶ。
    pub async fn clear_room(&self, room_id: &RoomId) {
        let mut guard = self.inner.lock().await;
        guard.remove(room_id);
    }
}

impl Broadcaster for InMemoryBroadcaster {
    async fn broadcast_room(&self, room_id: &RoomId, line: &CsaLine) -> Result<(), TransportError> {
        // `run_loop` 側で対局者への二重配信を避けるため `broadcast_room` は使わない
        // 規約にしてあるが、trait 契約上は用意しておく。
        self.broadcast_tag(room_id, BroadcastTag::Spectator, line).await
    }

    async fn broadcast_tag(
        &self,
        room_id: &RoomId,
        tag: BroadcastTag,
        line: &CsaLine,
    ) -> Result<(), TransportError> {
        if !matches!(tag, BroadcastTag::Spectator) {
            // Phase 1 では Admin/Player タグは使われない。未対応経路が来たら黙って no-op。
            return Ok(());
        }
        let mut guard = self.inner.lock().await;
        let Some(subs) = guard.get_mut(room_id) else {
            return Ok(());
        };
        // 受信側が停止している subscriber は `send` に失敗するので、ここで掃除する。
        subs.retain(|s| s.tx.send(line.clone()).is_ok());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_tag_spectator_reaches_registered_subscriber() {
        let bcast = InMemoryBroadcaster::new();
        let (tx, mut rx) = unbounded_channel();
        bcast.subscribe(RoomId::new("g1"), Subscriber::new(tx)).await;

        bcast
            .broadcast_tag(&RoomId::new("g1"), BroadcastTag::Spectator, &CsaLine::new("HELLO"))
            .await
            .unwrap();

        let got = rx.recv().await.unwrap();
        assert_eq!(got.as_str(), "HELLO");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_tag_non_spectator_is_no_op_in_phase1() {
        let bcast = InMemoryBroadcaster::new();
        let (tx, mut rx) = unbounded_channel();
        bcast.subscribe(RoomId::new("g1"), Subscriber::new(tx)).await;
        bcast
            .broadcast_tag(&RoomId::new("g1"), BroadcastTag::Player, &CsaLine::new("X"))
            .await
            .unwrap();
        // Player タグは Phase 1 では無視される。
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_tag_prunes_dead_subscribers() {
        let bcast = InMemoryBroadcaster::new();
        let (tx, rx) = unbounded_channel::<CsaLine>();
        bcast.subscribe(RoomId::new("g1"), Subscriber::new(tx)).await;
        drop(rx); // 受信側を先に捨てる → 送信は以降ずっと Err
        bcast
            .broadcast_tag(&RoomId::new("g1"), BroadcastTag::Spectator, &CsaLine::new("X"))
            .await
            .unwrap();
        // dead subscriber は掃除されているので、内部 Vec は空になっている。
        let guard = bcast.inner.lock().await;
        assert!(guard.get(&RoomId::new("g1")).unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_room_unknown_room_is_ok() {
        let bcast = InMemoryBroadcaster::new();
        bcast
            .broadcast_tag(&RoomId::new("unknown"), BroadcastTag::Spectator, &CsaLine::new("X"))
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn clear_room_removes_all_subscribers() {
        let bcast = InMemoryBroadcaster::new();
        let (tx, mut rx) = unbounded_channel();
        bcast.subscribe(RoomId::new("g1"), Subscriber::new(tx)).await;
        bcast.clear_room(&RoomId::new("g1")).await;
        bcast
            .broadcast_tag(&RoomId::new("g1"), BroadcastTag::Spectator, &CsaLine::new("X"))
            .await
            .unwrap();
        assert!(rx.try_recv().is_err());
    }
}
