# Transposition Table Architecture

本エンジンの置換表（TT）は「単一テーブル＋バケット（クラスター）構造」に統一されています。YaneuraOuの設計思想に近づけ、実運用での安定性と簡潔さを優先しています。

## 目的と方針

- 単一テーブル: シャーディングを行わず、全体で一貫した整合性を維持
- キャッシュ効率: 1バケット=64Bに収まる固定構造（4エントリ程度）で局所性を確保
- ロックフリー: 値→メタ→キー（公開フラグ）順の発行で安全に更新
- 世代管理: 年齢（age）で古いエントリを自然淘汰。インクリメンタルGCで負荷分散
- PV再構築: EXACT連鎖のみ辿り、非EXACT・不一致・非合法で停止

## 主要API（`search::tt::TranspositionTable`）

- `new(size_mb)` / `new_with_config(size_mb, bucket_size)`
- `probe(hash) -> Option<TTEntry>`: ハッシュ一致のみ有効。値は16bitスコア＋16bit評価＋深さ＋型
- `store(hash, mv, score, eval, depth, node_type)` / `store_with_params(params)`
- `set_exact_cut(hash)` / `clear_exact_cut(hash)`: ABDADA用フラグ
- `hashfull() -> u16`: 推定占有率（permille）
- `perform_incremental_gc(batch)` / `new_search()`: GC・世代更新
- `reconstruct_pv_from_tt(&mut pos, max_depth) -> Vec<Move>`: EXACT連鎖のPV

## チューニング指針

- TTサイズ: 2のべき乗（16/32/64/128MB…）を推奨
- GC: `perform_incremental_gc`を探索中の節目で呼び出し可能（既定の閾値で`need_gc`が立つ）
- スコア: TT格納時は`adjust_mate_score_for_tt`でroot相対化、取得時は`adjust_mate_score_from_tt`で復元
- PV再構築: 非EXACT・浅い深さ（<4）・不一致検出で早期停止し、TT汚染の影響を抑制

## ロギングとメトリクス

- `hashfull()`: 占有率の把握
- 送信ログ（TSV）: `kind=bestmove_sent` の `nps`、`kind=bestmove_metrics` の `pv_len`/`ponder_source` と併用
- 解析ツール: `tools/metrics_analyzer.rs` でPonder率・PV長・NPSを集計

## 移行ノート

- 旧: `ShardedTranspositionTable` は廃止
- 現: すべて `TranspositionTable` に統一
- 影響: PV再構築や整合検証が単純化。高スレッド数（数百コア）用途で必要になれば、将来的に代替配置（2ハッシュ）等で拡張
