//! 進行中対局のレジストリ。
//!
//! `%%LIST` / `%%SHOW` 応答や、観戦経路での対局メタデータ取得に使う。
//! `GameRoom` 自体のライフサイクルとは独立した「サマリ情報のスナップショット」を
//! 保持する。フロントエンドが対局を start/finish する際に明示的に
//! [`GameRegistry::register`] / [`GameRegistry::unregister`] を呼ぶ運用にする。
//!
//! 棋譜そのもの（指し手列）はここに保持しない。指し手列は `GameRoom` が
//! in-memory で、永続棋譜は `KifuStorage` が、それぞれの寿命で管理する。
//! `GameRegistry` は「誰と誰がどの game_name で何時から対局中か」までを覚える。
//!
//! 観戦の購読管理 (MONITOR2ON / MONITOR2OFF) は別モジュールで持ち、
//! レジストリは読み取り側の情報源として参照だけされる想定。

use std::collections::HashMap;

use crate::types::{GameId, GameName, PlayerName};

/// 1 対局分のサマリ（進行中にアクセスする範囲）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameListing {
    /// 対局 ID（`20140101120000-0001` 等）。
    pub game_id: GameId,
    /// 先手プレイヤ名。
    pub black: PlayerName,
    /// 後手プレイヤ名。
    pub white: PlayerName,
    /// `game_name`（`floodgate-600-10` 等）。
    pub game_name: GameName,
    /// 対局開始時刻（ISO 8601）。
    pub started_at: String,
}

/// 進行中対局のインメモリレジストリ。
///
/// 登録の加速はクレート利用側の合計対局数が十分小さい（数十〜数千）ことを
/// 前提にして `HashMap<GameId, GameListing>` 1 つで済ませる。
#[derive(Debug, Default)]
pub struct GameRegistry {
    games: HashMap<GameId, GameListing>,
}

impl GameRegistry {
    /// 空のレジストリを作る。
    pub fn new() -> Self {
        Self::default()
    }

    /// 1 対局を登録する。既に同じ `game_id` が登録されていれば上書き。
    pub fn register(&mut self, listing: GameListing) {
        self.games.insert(listing.game_id.clone(), listing);
    }

    /// 登録を外す。未登録の `game_id` を渡しても no-op。
    pub fn unregister(&mut self, game_id: &GameId) {
        self.games.remove(game_id);
    }

    /// `game_id` で対局サマリを引く。
    pub fn get(&self, game_id: &GameId) -> Option<&GameListing> {
        self.games.get(game_id)
    }

    /// 全対局のスナップショットを `game_id` 昇順で返す。
    ///
    /// `%%LIST` 応答では決定論的な順序で流したいので、呼び出し側でソートを
    /// 書かなくて済むようにここでソート済みの Vec を返す。
    pub fn snapshot(&self) -> Vec<GameListing> {
        let mut v: Vec<GameListing> = self.games.values().cloned().collect();
        v.sort_by(|a, b| a.game_id.as_str().cmp(b.game_id.as_str()));
        v
    }

    /// 登録数。
    pub fn len(&self) -> usize {
        self.games.len()
    }

    /// 空かどうか。
    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(game_id: &str, black: &str, white: &str, game_name: &str) -> GameListing {
        GameListing {
            game_id: GameId::new(game_id),
            black: PlayerName::new(black),
            white: PlayerName::new(white),
            game_name: GameName::new(game_name),
            started_at: "2026-04-17T12:00:00Z".to_owned(),
        }
    }

    #[test]
    fn register_and_snapshot_is_sorted_by_game_id() {
        let mut r = GameRegistry::new();
        r.register(listing("g-2", "c", "d", "g1"));
        r.register(listing("g-1", "a", "b", "g1"));
        let snap = r.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].game_id.as_str(), "g-1");
        assert_eq!(snap[1].game_id.as_str(), "g-2");
    }

    #[test]
    fn register_same_game_id_overwrites() {
        let mut r = GameRegistry::new();
        r.register(listing("g-1", "a", "b", "g1"));
        r.register(listing("g-1", "x", "y", "g2"));
        assert_eq!(r.len(), 1);
        assert_eq!(r.get(&GameId::new("g-1")).unwrap().black.as_str(), "x");
        assert_eq!(r.get(&GameId::new("g-1")).unwrap().game_name.as_str(), "g2");
    }

    #[test]
    fn unregister_is_idempotent() {
        let mut r = GameRegistry::new();
        r.register(listing("g-1", "a", "b", "g1"));
        r.unregister(&GameId::new("g-1"));
        r.unregister(&GameId::new("g-1")); // 2 度目は no-op
        assert!(r.is_empty());
        assert!(r.get(&GameId::new("g-1")).is_none());
    }

    #[test]
    fn empty_snapshot_is_empty_vec() {
        let r = GameRegistry::new();
        assert!(r.snapshot().is_empty());
    }
}
