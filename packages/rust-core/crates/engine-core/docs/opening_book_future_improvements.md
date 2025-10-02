# Opening Book Future Improvements

## 手番考慮のハッシュ生成

### 現状
現在の実装では、定跡データベースのハッシュ生成時に手番（先手/後手）を無視しています。これは意図的な設計判断で、盤面と手駒の情報のみでポジションを識別しています。

**技術的背景：**
- 開閉定跡（opening book）のキーに手番ビットを含めるのは将棋/チェス系エンジンでは標準的
- 現状では「同一の盤・持ち駒だが手番が異なる」局面のデータが混在する可能性がある
- 特に複数の棋譜ソースから定跡を生成する場合、同じ配置に異なる経路で到達することがあり、手番を無視すると片側の指し手や統計が上書き/混入される

### 将来的な改善案

#### 1. 基本実装方針

**`zobrist_turn` フィールドの活用**
- `PositionHasher` 構造体の `zobrist_turn` フィールドを使用して手番を区別
- コメントアウトされているコード（position_hasher.rs:131-141）を有効化

**実装方法**
```rust
// 型安全な実装への改善案
enum Turn { Black, White }

fn book_hash(&self, pos: &Position, turn: Turn) -> u64 {
    let mut hash = self.hash_board_and_hands(pos);
    if matches!(turn, Turn::White) {
        hash ^= self.zobrist_turn;
    }
    hash
}
```

#### 2. 実装上の重要事項

**Zobrist定数の固定化**
- 定跡DBのキーに使う Zobrist テーブルと `zobrist_turn` は**ビルド間で不変の固定定数**にする必要がある
- 現在の実装のようなランダム生成ではなく、固定seed（例：`0xDEADBEEF`）を使用
- これによりビルド/起動でキーが変わることを防ぐ

**エンジン内部との一貫性**
- ゲームエンジンの Zobrist（zobrist.rs）は既に手番を含んでいる
- 定跡用とエンジン用で異なる規約を持つ場合は、明確に分離し文書化する

#### 3. データベース移行戦略

**フォーマットバージョニング**
```rust
struct BookHeader {
    magic: [u8; 4],      // "SFEN"
    format_version: u16,  // 1: 旧形式（手番無視）, 2: 新形式（手番考慮）
    position_count: u64,
    checksum: u64,
    // 新規追加フィールド
    zobrist_seed_id: u64, // 使用したZobrist定数セットの識別子
    created_at: u64,      // タイムスタンプ
}
```

**互換性維持のための段階的移行**
1. 新キーでヒットしなければ旧キーでも検索するフォールバック機能
2. 移行期間中は両形式のDBを共存させる
3. 旧DBから新DBへの変換ツールを提供

```rust
// 移行用のキー再マッピング
fn migrate_key(old_key: u64, turn: Turn, zobrist_turn: u64) -> u64 {
    match turn {
        Turn::Black => old_key,  // 先手は変更なし
        Turn::White => old_key ^ zobrist_turn,  // 後手のみXOR
    }
}
```

#### 4. 影響範囲と対応

**コード変更**
- `test_different_turn_same_hash` テストを `test_different_turn_produces_different_hash` に変更
- 既存の定跡データベースとの互換性処理を追加
- パフォーマンステストで影響を測定

**データベースサイズ**
- ユニークキーが最大で約2倍になる可能性
- 実際には「同一配置で手番だけが異なる」ケースは限定的なので、増加率は10-30%程度と予想

#### 5. テスト観点

追加すべきテスト：
- `test_same_board_same_hands_diff_turn_produces_diff_hash`: 手番違いで必ずハッシュが変わることを確認
- `test_round_trip_book_lookup`: SFEN（含手番）→ ハッシュ → 検索 → 期待の指し手集合が返ることを確認
- `test_migration_preserves_data`: 旧形式から新形式への変換で情報が失われないことを確認
- `test_fallback_search`: 新キーで未ヒット時に旧キーでの検索が機能することを確認

### 実装タイミング

以下の条件が満たされた場合に実装を推奨：
- 手番を無視することで実際に問題が発生したケースが報告される
- より精密な定跡データベースが必要とされる
- データベースの再生成コストが許容される
- 外部配布済みのDBがない、もしくは少ない段階（早期実装が望ましい）

## その他の考慮事項

### キー幅の拡張（オプション）
長期保守を考慮して、以下の拡張も検討可能：
- `u128` キーの採用
- 「`u64 key` + `u64 checksum`」の二段キー構成
- 衝突時の誤混入リスクをさらに低減

### メタ情報の充実
BookHeaderに以下を追加：
- ソース情報（棋譜集名、エンジン名、生成パラメータ）
- 統計情報（総局面数、平均分岐数など）
- 圧縮形式やエンコーディング情報