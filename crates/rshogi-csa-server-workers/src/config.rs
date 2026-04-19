//! ランタイム設定の読み取りヘルパ。
//!
//! Workers の `[vars]` / secret から値を取り出すロジックを worker ランタイムから
//! 分離してテスト可能にする。値取得の実体は wasm32 ビルドでのみ行い、
//! 本モジュールが返すのは「取得結果から導出した純粋データ」に閉じる。

use crate::origin;

/// 起動時にバインディング名として参照する環境変数キー群。
pub struct ConfigKeys;

impl ConfigKeys {
    /// Origin 許可リスト（カンマ区切り）。
    pub const CORS_ORIGINS: &'static str = "CORS_ORIGINS";
    /// Durable Object バインディング名（GameRoom 1 対局 = 1 インスタンス）。
    pub const GAME_ROOM_BINDING: &'static str = "GAME_ROOM";
    /// R2 バケットバインディング名（CSA V2 棋譜保存）。
    pub const KIFU_BUCKET_BINDING: &'static str = "KIFU_BUCKET";
}

/// 取得済みの Origin 許可リスト設定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginAllowList {
    entries: Vec<String>,
}

impl OriginAllowList {
    /// CSV（例: `"https://a.example,https://b.example"`）から構築する。
    pub fn from_csv(csv: &str) -> Self {
        Self {
            entries: origin::parse_allow_list(csv),
        }
    }

    /// 空かどうか。本番運用で空は実質全拒否となる（[`origin::evaluate`] の仕様）。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 許可リストをイテレートする。
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_csv_yields_empty_list() {
        let list = OriginAllowList::from_csv("");
        assert!(list.is_empty());
    }

    #[test]
    fn csv_parsing_round_trips() {
        let list = OriginAllowList::from_csv("https://a.example, https://b.example");
        let collected: Vec<&str> = list.iter().collect();
        assert_eq!(collected, vec!["https://a.example", "https://b.example"]);
    }
}
