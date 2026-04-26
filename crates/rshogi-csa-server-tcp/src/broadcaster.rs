//! 同一プロセス内の観戦者接続を束ねる in-memory [`Broadcaster`] 実装。
//!
//! 観戦者購読が実体化されていない本バイナリ構成でも動けるよう、副作用が
//! 起きても安全なスタブを用意する。`run_room` は `BroadcastTag::Spectator`
//! を必ず 1 回以上呼ぶので、呼び先が空の実装でも例外にならない契約にしている。
//!
//! 設計上、各ルームの観戦者集合は `Mutex<HashMap<RoomId, Vec<Subscriber>>>` で保持する。
//! 1 subscriber あたり [`tokio::sync::mpsc::Sender<CsaLine>`] (**bounded**) を持たせ、
//! 実際の送信は受信側タスクが担う（I/O をロック内で行わないようにするため）。
//!
//! # Bounded channel
//! `UnboundedSender` を使うと slow-but-not-dead な observer が無制限にキューを
//! 溜め込むメモリ肥大経路になり得る。`%%CHAT` はユーザー駆動で rate-limit が
//! 無く、1 観戦者あたりの buffer を緩和しないと DoS リスクになる。本版では
//! 容量 [`SUBSCRIBER_CHANNEL_CAPACITY`] の bounded channel を使い、`try_send`
//! が失敗した subscriber は「配信不能」とみなして broadcaster 側の retain で
//! 即 prune する (disconnect と同等の扱い)。これによりプロセス全体の
//! buffer 上限が `room 数 × subscriber 数 × capacity × 1 行最大サイズ` に収まる。

use std::collections::HashMap;
use std::sync::Arc;

use rshogi_csa_server::TransportError;
use rshogi_csa_server::port::{BroadcastTag, Broadcaster};
use rshogi_csa_server::types::{CsaLine, RoomId};
use tokio::sync::Mutex;
use tokio::sync::mpsc::Sender;

/// 1 subscriber あたりの broadcast キュー容量（行数）。
///
/// 通常の対局では 1 手に 1 行、チャットを含めても 1 秒あたり数行が上限。
/// 256 行は数分〜数十分の受信遅延を吸収できる余裕で、かつ 1 観戦者あたりの
/// メモリ最大値を `256 * sizeof(CsaLine)` に抑える。
pub const SUBSCRIBER_CHANNEL_CAPACITY: usize = 256;

/// 1 人分の観戦者ハンドル。
#[derive(Clone)]
pub struct Subscriber {
    /// 受信側タスクへ 1 行を送る送信口 (bounded)。送信失敗 (受信タスク停止 /
    /// キューあふれ) は `retain` で掃除される。
    tx: Sender<CsaLine>,
}

impl Subscriber {
    /// 与えられた送信口で Subscriber を作る。
    pub fn new(tx: Sender<CsaLine>) -> Self {
        Self { tx }
    }
}

/// プロセスローカルの `Broadcaster`。
///
/// 1 プロセスに 1 インスタンスだけ作り、`Arc` で共有する想定。複数プロセス間の
/// 配信はこのクレートの責務外（別フロントエンドが受け持つ）。
#[derive(Default, Clone)]
pub struct InMemoryBroadcaster {
    inner: Arc<Mutex<HashMap<RoomId, Vec<Subscriber>>>>,
}

impl InMemoryBroadcaster {
    /// 空のブロードキャスタを作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// ルームに観戦者を登録する（`%%MONITOR2ON` 相当の拡張経路から呼ばれる想定）。
    ///
    /// 登録の前に既存 subscriber の中で `tx.is_closed()` が true (= receiver が
    /// drop 済み) のものを retain で prune する。`%%MONITOR2OFF` や購読先切替で
    /// 行き場の無くなった Sender が `clear_room` まで残らないようにするため。
    /// broadcast が発生しない idle な room でも登録時に毎回掃除されるので、
    /// dead entry の蓄積は O(同時購読者数) に抑えられる。
    pub async fn subscribe(&self, room_id: RoomId, subscriber: Subscriber) {
        let mut guard = self.inner.lock().await;
        let entry = guard.entry(room_id).or_default();
        entry.retain(|s| !s.tx.is_closed());
        entry.push(subscriber);
    }

    /// 指定ルームに紐づく dead subscriber (receiver drop 済み) を prune する。
    ///
    /// `%%MONITOR2OFF` など「subscribe は呼ばないが掃除はしたい」経路で使う。
    /// broadcast 経路と違い `retain` は `is_closed` のみを基準にするため、
    /// 生存中の subscriber への送信は発生しない。
    pub async fn prune_closed(&self, room_id: &RoomId) {
        let mut guard = self.inner.lock().await;
        if let Some(subs) = guard.get_mut(room_id) {
            subs.retain(|s| !s.tx.is_closed());
        }
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
            // Admin/Player タグは本実装では使われない。未対応経路が来たら黙って no-op。
            return Ok(());
        }
        let mut guard = self.inner.lock().await;
        let Some(subs) = guard.get_mut(room_id) else {
            return Ok(());
        };
        // `try_send` は (a) 受信側停止、(b) キューあふれ のどちらでも Err を返す。
        // どちらも「配信不能」とみなし、retain で即 prune する。これにより slow
        // consumer が全体 memory を溜め込む経路を遮断する (broadcast loss の責務は
        // subscriber 側が draw back する設計: observer 側で一時的に受信が止まれば
        // broadcaster から切断される)。
        subs.retain(|s| s.tx.try_send(line.clone()).is_ok());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::channel;

    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_tag_spectator_reaches_registered_subscriber() {
        let bcast = InMemoryBroadcaster::new();
        let (tx, mut rx) = channel(SUBSCRIBER_CHANNEL_CAPACITY);
        bcast.subscribe(RoomId::new("g1"), Subscriber::new(tx)).await;

        bcast
            .broadcast_tag(&RoomId::new("g1"), BroadcastTag::Spectator, &CsaLine::new("HELLO"))
            .await
            .unwrap();

        let got = rx.recv().await.unwrap();
        assert_eq!(got.as_str(), "HELLO");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_tag_non_spectator_is_no_op() {
        let bcast = InMemoryBroadcaster::new();
        let (tx, mut rx) = channel(SUBSCRIBER_CHANNEL_CAPACITY);
        bcast.subscribe(RoomId::new("g1"), Subscriber::new(tx)).await;
        bcast
            .broadcast_tag(&RoomId::new("g1"), BroadcastTag::Player, &CsaLine::new("X"))
            .await
            .unwrap();
        // Player タグは本実装では無視される。
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn broadcast_tag_prunes_dead_subscribers() {
        let bcast = InMemoryBroadcaster::new();
        let (tx, rx) = channel::<CsaLine>(SUBSCRIBER_CHANNEL_CAPACITY);
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
    async fn broadcast_tag_prunes_full_subscribers() {
        // slow consumer がキューをあふれさせた subscriber は try_send が WouldBlock
        // で失敗し、即 prune されることを確認する。bounded channel の capacity を 1
        // にして、1 通目までは受理、2 通目で overflow → prune。
        let bcast = InMemoryBroadcaster::new();
        let (tx, _keep_rx) = channel::<CsaLine>(1);
        bcast.subscribe(RoomId::new("g1"), Subscriber::new(tx)).await;
        // 1 通目は受理されキューに積まれる。subscriber はまだ生存。
        bcast
            .broadcast_tag(&RoomId::new("g1"), BroadcastTag::Spectator, &CsaLine::new("1"))
            .await
            .unwrap();
        assert_eq!(bcast.inner.lock().await.get(&RoomId::new("g1")).unwrap().len(), 1);
        // 2 通目は try_send が Full で失敗 → subscriber が prune される。
        bcast
            .broadcast_tag(&RoomId::new("g1"), BroadcastTag::Spectator, &CsaLine::new("2"))
            .await
            .unwrap();
        assert_eq!(bcast.inner.lock().await.get(&RoomId::new("g1")).unwrap().len(), 0);
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
        let (tx, mut rx) = channel(SUBSCRIBER_CHANNEL_CAPACITY);
        bcast.subscribe(RoomId::new("g1"), Subscriber::new(tx)).await;
        bcast.clear_room(&RoomId::new("g1")).await;
        bcast
            .broadcast_tag(&RoomId::new("g1"), BroadcastTag::Spectator, &CsaLine::new("X"))
            .await
            .unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn subscribe_prunes_closed_subscribers_before_inserting() {
        // 観戦者が MONITOR2OFF / 切替で rx を drop しても、broadcast が発火するまで
        // 死んだ Sender が残る。subscribe が次の購読登録の直前に dead entry を
        // prune することで、反復トグルによる蓄積を O(同時生存購読者数) に抑える。
        let bcast = InMemoryBroadcaster::new();
        let (tx1, rx1) = channel::<CsaLine>(SUBSCRIBER_CHANNEL_CAPACITY);
        bcast.subscribe(RoomId::new("g1"), Subscriber::new(tx1)).await;
        drop(rx1); // 1 つ目の subscriber を dead 化
        // broadcast 無しで 2 つ目を subscribe。1 つ目は prune されるはず。
        let (tx2, _rx2) = channel::<CsaLine>(SUBSCRIBER_CHANNEL_CAPACITY);
        bcast.subscribe(RoomId::new("g1"), Subscriber::new(tx2)).await;
        let guard = bcast.inner.lock().await;
        let subs = guard.get(&RoomId::new("g1")).unwrap();
        assert_eq!(subs.len(), 1, "subscribe should have pruned dead entry first");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prune_closed_removes_dead_entries_on_demand() {
        // MONITOR2OFF 経路では subscribe は呼ばれないため、専用の prune_closed で
        // 明示的に dead entry を掃除する。broadcast 無しで直接 prune が走って
        // いることを確認。
        let bcast = InMemoryBroadcaster::new();
        let (tx, rx) = channel::<CsaLine>(SUBSCRIBER_CHANNEL_CAPACITY);
        bcast.subscribe(RoomId::new("g1"), Subscriber::new(tx)).await;
        drop(rx);
        assert_eq!(bcast.inner.lock().await.get(&RoomId::new("g1")).unwrap().len(), 1);
        bcast.prune_closed(&RoomId::new("g1")).await;
        assert_eq!(bcast.inner.lock().await.get(&RoomId::new("g1")).unwrap().len(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prune_closed_on_unknown_room_is_noop() {
        // 存在しない room_id に対する prune は no-op (エラーにならない)。
        let bcast = InMemoryBroadcaster::new();
        bcast.prune_closed(&RoomId::new("never-subscribed")).await;
    }
}
