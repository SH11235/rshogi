//! 置換表プリフェッチのトレイト定義
//!
//! 探索中に次の局面の置換表エントリを事前にキャッシュに読み込むことで、
//! メモリアクセスのレイテンシを隠蔽します。

use crate::types::Color;

/// 置換表のプリフェッチを行うトレイト
///
/// 探索中に `do_move` 実行時に次の局面のTTエントリをプリフェッチすることで、
/// 実際のTT参照時にはキャッシュにヒットしやすくなります。
/// YaneuraOuのプリフェッチタイミングに準拠しています。
pub(crate) trait TtPrefetch {
    /// 指定されたキーと手番に対応する置換表エントリをプリフェッチする
    fn prefetch(&self, key: u64, side_to_move: Color);
}

/// プリフェッチを行わないダミー実装
///
/// 探索以外の用途（局面生成、棋譜再生、テストなど）で使用します。
/// TTが初期化されていない場合や、プリフェッチが不要な場面で利用されます。
pub(crate) struct NoPrefetch;

impl TtPrefetch for NoPrefetch {
    #[inline]
    fn prefetch(&self, _key: u64, _side_to_move: Color) {}
}
