//! rshogi 定跡(opening book)機構 Phase 1。
//!
//! YANEURAOU-DB2016 テキスト `.db` 形式(外部で広く流通する定跡 DB フォーマット)の
//! リーダと、root 局面 1 回の probe(指し手選択)を提供する。ファイルフォーマットのみ
//! 外部互換とし、probe 機構・実装は rshogi 内で完結する。
//!
//! # 概要
//!
//! - [`Book`]: `.db` を丸読みした定跡本体。path 版([`Book::from_path`])と
//!   bytes 版([`Book::from_bytes`])の二本立て API(NNUE ローダ準拠、wasm 互換)。
//! - [`probe`]: root 局面に対して定跡手を選ぶ。miss 時は [`BookOptions::flipped_book`]
//!   により先後反転局面で再検索する(FlippedBook)。
//! - [`BookOptions`]: USI オプションのミラー。
//! - [`BookRng`] / [`DefaultBookRng`]: 抽選用乱数源(テストで固定注入可能)。

mod explore;
mod flip;
mod probe;
mod reader;

pub use explore::BookExploreList;
pub use probe::{BookOptions, BookProbeResult, BookRng, DefaultBookRng, probe, probe_with_explore};
pub use reader::Book;

pub use flip::{flip_usi_move, flipped_key};
