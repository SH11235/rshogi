---
description: rshogi の指定モジュールを YaneuraOu の対応実装と比較し、差分をレビューする
user-invocable: true
---

# YaneuraOu 比較コードレビュー

rshogi の指定領域を YaneuraOu (YO) の対応実装と行単位で対照比較し、差分レポートを生成する。
コード変更は行わない読み取り専用スキル。

## 入力パラメータ

`$ARGUMENTS` で比較対象の領域を受け取る。

例:
- `MovePicker scoring`
- `qsearch delta pruning`
- `TT replacement policy`
- `Silver CheckCandidateBB`
- `alpha_beta futility pruning`

引数が空または曖昧な場合は、ファイル対応表を提示してユーザーに選択を求めること。

## ファイル対応表

rshogi のルートパス: `crates/rshogi-core/src/`
YO のベースパス: `/mnt/nvme1/development/YaneuraOu/source/`

**重要**: YO ソースは必ず上記の正規パスを使用すること。

| rshogi | YO | 領域 |
|---|---|---|
| `search/alpha_beta.rs` | `engine/yaneuraou-engine/yaneuraou-search.cpp` | alpha-beta 探索メインループ |
| `search/qsearch.rs` | `engine/yaneuraou-engine/yaneuraou-search.cpp` (qsearch 関数) | 静止探索 |
| `search/pruning.rs` | `engine/yaneuraou-engine/yaneuraou-search.cpp` (pruning 部分) | 枝刈り |
| `search/movepicker.rs` | `movepick.cpp`, `movepick.h` | MovePicker ステートマシン・スコアリング |
| `search/history.rs` | `history.h` | ヒストリテーブル (Butterfly, Continuation, Pawn, Capture) |
| `search/tune_params.rs` | `engine/yaneuraou-engine/yaneuraou-search.cpp` (インライン定数) | 探索パラメータ定数 |
| `search/search_helpers.rs` | `engine/yaneuraou-engine/yaneuraou-search.cpp` | stat_bonus/malus, update_all_stats 等 |
| `tt/entry.rs`, `tt/table.rs` | `tt.cpp`, `tt.h` | 置換表 |
| `movegen/generator.rs` | `movegen.cpp` | 手生成 |
| `mate/` (`drop_mate.rs`, `move_mate.rs`, `helpers.rs`, `tables.rs`) | `mate/mate1ply_without_effect.cpp` | 1手詰め |
| `types/` (`piece.rs`, `piece_type.rs`, `moves.rs`, `square.rs` 等) | `types.h`, `position.h` | 基本型定義 |
| `bitboard/` (`core.rs`, `tables.rs`, `check_candidate.rs`, `sliders.rs`) | `bitboard.cpp`, `bitboard.h` | Bitboard |
| `position/` (`mod.rs`, `state.rs`, `sfen.rs`) | `position.cpp`, `position.h` | 局面管理 |
| `eval/` | `eval/evaluate_bona_piece.cpp` | 評価関数 |

## 比較手順

### Step 1: rshogi 側コードの特定・読み取り

1. `$ARGUMENTS` のキーワードをもとに、ファイル対応表から該当ファイルを特定
2. Grep で関数名・定数名を検索し、該当箇所を絞り込み
3. Read で該当コードを読み取り

### Step 2: YO 側コードの特定・読み取り

1. 対応表から YO 側のファイルを特定
2. Grep で同等の関数名・定数名を検索
3. Read で該当コードを読み取り

**YO ソースの注意点:**
- `#if STOCKFISH` / `#else` 分岐がある場合、`#else` 側（将棋固有パス）を参照すること
- `moved_piece(m)` は `moved_piece_after(m)` のエイリアス（成り後の駒、Stockfish とは異なる）
- `Depth = int` であり `ONE_PLY` は存在しない
- `ss->inCheck` 時に `goto moves_loop` でeval計算〜pruningが全スキップされる設計

### Step 3: 行単位の対照比較

rshogi と YO のコードを対照し、以下の観点で差分を抽出する:

1. **ロジックの一致**: 条件分岐、計算式、実行順序
2. **定数値の一致**: マジックナンバー、閾値、係数
3. **変数・型の対応**: 命名の違いを超えた意味的な一致
4. **ガード条件**: `in_check` ガード等の有無

### Step 4: 差分分類

検出した差分を以下のカテゴリに分類する:

| カテゴリ | 説明 | 対応 |
|---|---|---|
| **BUG** | YO と異なる動作をするバグ | 即修正が必要 |
| **NAMING** | 命名が異なるだけで動作は同一 | 対応不要 |
| **INTENTIONAL** | rshogi 側で意図的に変更した部分 | 理由を記載 |
| **YO_SPECIFIC** | YO 固有の機能で rshogi に不要 | 対応不要 |
| **MISSING** | rshogi に未実装の YO 機能 | 要検討 |

## 出力フォーマット

以下のフォーマットでレポートを出力すること:

```markdown
## YO 比較レポート: {対象領域}

### 比較範囲

- **rshogi**: `{ファイルパス}` L{開始}-L{終了}
- **YO**: `{ファイルパス}` L{開始}-L{終了}

### 対照表

| # | 項目 | rshogi | YO | 一致 |
|---|------|--------|-----|------|
| 1 | {具体的な項目} | {コード/値} | {コード/値} | OK / DIFF |
| ... | ... | ... | ... | ... |

### 差分一覧

#### {差分1のタイトル}
- **カテゴリ**: BUG / NAMING / INTENTIONAL / YO_SPECIFIC / MISSING
- **rshogi**: `{コード}` ({ファイル}:L{行})
- **YO**: `{コード}` ({ファイル}:L{行})
- **影響**: {動作への影響の説明}
- **推奨対応**: {対応方針}

### 総合判定

- 一致項目数: {N} / {total}
- BUG: {件数}
- MISSING: {件数}
- 総合: {PASS / REVIEW_NEEDED / FIX_REQUIRED}
```

## 大規模な比較の場合

対象領域が広い場合（alpha_beta 全体、mate 全体など）は、Task ツールで Explore エージェントを活用し、
rshogi 側と YO 側の読み取りを並列化して効率的に比較すること。
