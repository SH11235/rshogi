---
name: yo-compare
description: "rshogi の指定モジュールを YaneuraOu の対応実装と比較し、差分をレビューする。「YO と比較して」「YO 準拠か確認して」「YaneuraOu との差分を見て」「YO と同じ挙動か」等のリクエストで使う。"
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
YO のベースパス: YaneuraOu checkout の `source/`。環境変数 `$YO_DIR` が設定されていれば
`$YO_DIR/source/` を使い、無ければユーザーに checkout の場所を聞く。推測したパスや
別バージョンの YO を読まない。比較の基準にする YO の commit / branch はユーザーの指定に従い、
指定が無ければ checkout の現在の HEAD を基準にする。どちらの場合も
`git -C "$YO_DIR" rev-parse HEAD` と `git -C "$YO_DIR" branch --show-current` の結果をレポートに記録する。

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
| `position/` (`pos.rs`, `state.rs`, `sfen.rs`, `zobrist.rs` 等) | `position.cpp`, `position.h` | 局面管理 |
| `eval/` (`material.rs`: 駒得・パス権評価, `eval_hash.rs`: 評価値キャッシュ) | `eval/evaluate_bona_piece.cpp` (対応は未確認。YO 側で Grep して確かめる) | 評価関数 (非 NNUE 部分) |
| `nnue/` (`feature_transformer*.rs`, `layer_stacks.rs`, `network_*.rs`, `bona_piece*.rs` 等) | 未確認 (YO の `eval/` 配下で Grep して特定する) | NNUE 評価 |

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
- `#if STOCKFISH` / `#else` の分岐は、将棋の build では `#else` 側 (将棋固有パス) が有効。edition 固有の define
  が絡む分岐は、比較対象の YO の build で有効な define から経路を判断して読む
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
| **UNVERIFIED** | 動作差があるが、意図した変更かの根拠が見つからない | BUG か INTENTIONAL かを user に確認 |

BUG と INTENTIONAL の判別は、次の証拠源で意図の有無を確かめてから行い、レポートに証拠を書く:

- 該当箇所と周辺のコメント (YO との差を説明しているか)
- `git log -L <開始>,<終了>:<ファイル>` / `git log -S '<コード片>'` で導入 commit を引き、commit message を読む
- `search/tune_params.rs` の値は、`SPSA_*` option としてチューニング対象か、導入 commit が SPSA 等の調整結果か YO からの転記かを `git log -S '<値>'` で確かめる

証拠が見つからない動作差は INTENTIONAL にも BUG にもせず UNVERIFIED に分類し、調べた証拠源を書く。

## 出力フォーマット

以下のフォーマットでレポートを出力すること:

```markdown
## YO 比較レポート: {対象領域}

### 比較範囲

- **rshogi**: `{ファイルパス}` L{開始}-L{終了} (commit `{rshogi の HEAD}`)
- **YO**: `{ファイルパス}` L{開始}-L{終了} (commit `{YO の HEAD}`, branch `{branch}`)

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
- **証拠**: {コメント / commit / tune_params の由来。見つからなければ「根拠未確認」}
- **影響**: {動作への影響の説明}
- **推奨対応**: {対応方針}

### 総合判定

- 一致項目数: {N} / {total}
- BUG: {件数}
- MISSING: {件数}
- UNVERIFIED: {件数}
- 総合: {PASS / REVIEW_NEEDED / FIX_REQUIRED}
```

総合判定の基準:

- **FIX_REQUIRED**: BUG が 1 件以上
- **REVIEW_NEEDED**: BUG が 0 件で、MISSING か UNVERIFIED が 1 件以上
- **PASS**: 上記のいずれにも当たらない (差分が NAMING / 証拠付き INTENTIONAL / YO_SPECIFIC だけ)

## YaneuraOu USI の落とし穴

比較結果を YO バイナリの実行で裏取りするときに踏みやすい点:

- 既定 `Threads=4`。単発の eval / NPS 計測では `Threads` を必ず明示する。
- pipe で `go` の直後に `quit` を送ると探索が即中断する。`bestmove` を待ってから `quit` を送る。
- 生の静的評価は `e` コマンド (`eval` ではない)。
- `d` の盤面出力は unicode を含むので grep には `-a` を付ける。

## 大規模な比較の場合

対象領域が広い場合（alpha_beta 全体、mate 全体など）は、並列のサブエージェントを使えるなら
rshogi 側と YO 側の読み取りを分担させて並列化する。
