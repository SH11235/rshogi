---
description: rshogi と YaneuraOu の探索ノード数乖離を調査する（A/Bテスト、発火回数カウント、PLY drill-down、デバッグログ挿入）
user-invocable: true
allowed-tools:
  - Read
  - Edit
  - Write
  - Grep
  - Glob
  - Bash
  - Task
---

# YO 計測デバッグスキル

rshogi と YaneuraOu (YO) の両エンジンにデバッグログを挿入→ビルド→計測→比較し、
探索ノード数乖離の原因を事実ベースで絞り込むスキル。

静的コード比較（yo-compare）では特定できない微細な乖離を、
計測データの差分から局所化する。

## 入力パラメータ

`$ARGUMENTS` で計測目標を受け取る。

例:
- `startpos d28 root move breakdown`
- `startpos d14 history at PLY3 path 4a3b/7g7f`
- `startpos d14 TT state at PLY2 path 4a3b`
- `startpos d14 SE nodes at PLY1`
- `pos1 d8 move ordering at PLY3`
- `line11818 d13 root move breakdown`
- `startpos d28 A/B test mate_1ply`

## ファイルパス

| 対象 | パス |
|------|------|
| rshogi 探索 | `crates/rshogi-core/src/search/alpha_beta.rs` |
| rshogi エンジン | `crates/rshogi-core/src/search/engine.rs` |
| rshogi qsearch | `crates/rshogi-core/src/search/qsearch.rs` |
| rshogi eval helpers | `crates/rshogi-core/src/search/eval_helpers.rs` |
| rshogi pruning | `crates/rshogi-core/src/search/pruning.rs` |
| rshogi mate | `crates/rshogi-core/src/mate/` (`mod.rs`, `move_mate.rs`, `drop_mate.rs`, `helpers.rs`) |
| rshogi position | `crates/rshogi-core/src/position/pos.rs` |
| YO 探索 | `/mnt/nvme1/development/YaneuraOu/source/engine/yaneuraou-engine/yaneuraou-search.cpp` |
| YO movepick | `/mnt/nvme1/development/YaneuraOu/source/movepick.cpp` |
| YO mate | `/mnt/nvme1/development/YaneuraOu/source/mate/mate1ply_without_effect.cpp` |
| YO position | `/mnt/nvme1/development/YaneuraOu/source/position.h`, `position.cpp` |
| 乖離ステータス | `docs/performance/yo_alignment_status.md` |
| ノード数比較 | `crates/tools/src/bin/compare_nodes.rs` (`cargo run --release -p tools --bin compare_nodes`) |
| NNUE eval | `/mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu/suisho5.bin` |
| rshogi binary | `target/release/rshogi-usi` |
| YO binary | `/mnt/nvme1/development/YaneuraOu/source/YaneuraOu-by-gcc` |

## 実行フロー

## 最短ルート（first mismatch 固定, 2026-02-23更新）

長時間化を避けるため、まずこの順で進めること。
実例は末尾の「過去の発見事例」セクションを参照。

1. **一致帯を先に確定**
   - `cargo run --release -p tools --bin compare_nodes -- --sfen "..." --depth N` で depth ごとの差分を出し、`d<=N` 一致 / `d=N+1` 初乖離を確定する。
2. **root 粒度で first mismatch を1件確定**
   - `iter, mc, mv, nd, val` の軽量ログのみ出す。
   - 最初にズレる1行（同一 iter / 同一手）を確定する。
3. **同一文脈ゲートで 1 ply ずつ drill-down**
   - `root_move + pm1 + pm2 + ply + depth (+必要なら window)` でゲート。
   - 各段で「最初にズレる child 1件」だけ追う。
4. **`val` より `nd` を優先**
   - fail-high/fail-low で `val` は揺れやすい。まず `nd` の最初の差分点を固定する。
5. **同一訪問確認を必須化**
   - key 不一致仮説を立てる前に、`parent_key/child_key + SFEN` を同時に出し、同一局面か確認する。
6. **TT は lifecycle で追う**
   - `seq, PROBE/WRITE, fullkey, key16, cluster_idx, slot_idx, old/new(depth/bound/value)` を時系列で採取。
   - これで「未書込」か「置換消失」かを短時間で確定できる。

### アンチパターン（避ける）

- `alpha/beta` 単独ゲート（別 iteration が混入しやすい）
- 全 return 箇所への一括ログ追加（ノイズ過多で遅い）
- 早い段階で静的コード差に寄りすぎる（まず実行時 first mismatch 固定）
- `key16` 一致のみで TT hit 妥当性を結論づける（`fullkey + cluster/slot` が必要）
- **`if-else if` チェインの途中にトレース用 `if` を挿入する**（チェインが壊れて探索ロジックが変わる。詳細は「デバッグコードで探索ロジックを壊さない」セクション参照）
- **デバッグコード入りビルドで「一致した」と結論する**（クリーンビルドで再確認必須）
- **pipe / file redirect でエンジンにコマンドを送る**（coproc 必須。詳細は「エンジン起動方法」セクション参照）

### Phase 1: 計測目標の明確化

1. `$ARGUMENTS` をパースし、以下を確定:
   - **局面**: startpos / SFEN 文字列
   - **探索深度**: depth N
   - **計測対象**: ログ種別（後述）
   - **ply パス**: ルートからの手順（例: `4a3b/7g7f/8c8d`）
   - **ノード範囲**: aspiration iteration を絞るためのノード数窓

2. 不明な項目があれば `yo_alignment_status.md` と直近の計測結果を参照して補完

### Phase 2: デバッグログの挿入

両エンジンに **対称な** デバッグログを挿入する。以下の規約に従うこと:

#### プレフィックス規約

```
RS_{ログ種別}   — rshogi 側 (eprintln!)
YO_{ログ種別}   — YaneuraOu 側 (std::cerr)
```

#### ログ種別テンプレート

##### ROOT_MOVE — ルート手ごとのノード数

rshogi（`alpha_beta.rs`、effort 加算直後）:
```rust
eprintln!(
    "RS_ROOT_MOVE depth={} mv={} nodes={} value={}",
    depth, mv.to_usi(), nodes_delta, value.raw()
);
```

YO（`yaneuraou-search.cpp`、`rm.effort += nodes - nodeCount` 直後）:
```cpp
std::cerr << "YO_ROOT_MOVE depth=" << depth
    << " mv=" << move.to_usi_string()
    << " nodes=" << (nodes - nodeCount)
    << " value=" << (int)value << std::endl;
```

##### ASP — aspiration window イテレーション

rshogi（`engine.rs`、`search_node` 呼び出し直後）:
```rust
eprintln!(
    "RS_ASP adj_depth={} alpha={} beta={} score={} nodes={}",
    adjusted_depth, alpha.raw(), beta.raw(), score.raw(), worker.state.nodes
);
```

YO（`yaneuraou-search.cpp`、`search<Root>` 呼び出し直後）:
```cpp
std::cerr << "YO_ASP adj_depth=" << adjustedDepth
    << " alpha=" << (int)alpha << " beta=" << (int)beta
    << " score=" << (int)bestValue
    << " nodes=" << nodes.load(std::memory_order_relaxed) << std::endl;
```

##### TT_PLY{N} — TT lookup 状態

rshogi（`alpha_beta.rs`、`tt_ctx` 取得直後）:
```rust
eprintln!(
    "RS_TT_PLY{} cum={} d={} hit={} mv={} val={} dep={} bound={:?}",
    ply, st.nodes, depth, tt_hit,
    if tt_move.is_some() { tt_move.to_usi() } else { "none".to_string() },
    tt_value.raw(), tt_data.depth, tt_data.bound
);
```

YO（`yaneuraou-search.cpp`、`ttCapture` 設定直前）:
```cpp
std::cerr << "YO_TT_PLY" << ss->ply
    << " cum=" << nodes.load(std::memory_order_relaxed)
    << " d=" << depth << " hit=" << ttHit
    << " mv=" << (ttData.move ? ttData.move.to_usi_string() : "none")
    << " val=" << (int)ttData.value
    << " dep=" << (int)ttData.depth
    << " bound=" << (int)ttData.bound << std::endl;
```

##### EVAL_PLY{N} — 静的評価

rshogi（`alpha_beta.rs`、`improving` 算出直後）:
```rust
eprintln!(
    "RS_EVAL_PLY{} cum={} d={} unadj={} eval={} corr={} improving={} ttpv={}",
    ply, st.nodes, depth,
    eval_ctx.unadjusted_static_eval.raw(),
    eval_ctx.eval.raw(), eval_ctx.correction_value,
    improving, st.stack[ply as usize].tt_pv,
);
```

YO（`yaneuraou-search.cpp`、`improving` 算出直後）:
```cpp
std::cerr << "YO_EVAL_PLY" << ss->ply
    << " cum=" << nodes.load(std::memory_order_relaxed)
    << " d=" << depth
    << " unadj=" << (int)unadjustedStaticEval
    << " eval=" << (int)eval
    << " corr=" << correctionValue
    << " improving=" << improving
    << " ttpv=" << ss->ttPv << std::endl;
```

##### SE — Singular Extension

rshogi（`alpha_beta.rs`、SE 再帰 search 直後）:
```rust
eprintln!(
    "RS_SE ply={} mv={} sd={} sb={} sv={} nodes={}",
    ply, mv.to_usi(), singular_depth, singular_beta.raw(),
    singular_value.raw(), st.nodes.saturating_sub(se_nodes_before)
);
```

YO（`yaneuraou-search.cpp`、SE search 直後）:
```cpp
std::cerr << "YO_SE ply=" << ss->ply
    << " mv=" << move.to_usi_string()
    << " sd=" << (int)singularDepth
    << " sb=" << (int)singularBeta
    << " sv=" << (int)value
    << " nodes=" << (nodes.load(std::memory_order_relaxed) - se_nodes_before)
    << std::endl;
```

##### PLY{N}_MOVE — 各 ply の手ごと結果

rshogi（`alpha_beta.rs`、`undo_move` 直後）:
```rust
eprintln!(
    "RS_PLY{N}_{tag} mv={} mc={} nodes={} value={} ext={} nd={} cum={}",
    mv.to_usi(), move_count,
    st.nodes.saturating_sub(nodes_before_move),
    value.raw(), extension, new_depth, st.nodes
);
```

YO（`yaneuraou-search.cpp`、`undo_move` 直後）:
```cpp
std::cerr << "YO_PLY{N}_{tag} mv=" << move.to_usi_string()
    << " mc=" << moveCount
    << " nodes=" << (nodes - ply1NodeCount)
    << " value=" << (int)value
    << " ext=" << (int)extension
    << " nd=" << (int)newDepth
    << " cum=" << nodes.load(std::memory_order_relaxed) << std::endl;
```

##### MATE1 — mate_1ply 発火ログ

mate_1ply の結果が RS/YO で異なる場合の調査に使用:

rshogi（`qsearch.rs`、mate_1ply 呼び出し後）:
```rust
if let Some(mate_move) = pos.mate_1ply() {
    eprintln!("RS_MATE1_QS key={:016x} ply={} found=1 mv={}", pos.key(), ply, mate_move.to_usi());
} else {
    eprintln!("RS_MATE1_QS key={:016x} ply={} found=0 mv=none", pos.key(), ply);
}
```

YO（`yaneuraou-search.cpp`、mate_1ply 呼び出し後）:
```cpp
auto m1 = pos.mate1ply();
if (m1) {
    std::cerr << "YO_MATE1_QS key=" << std::hex << pos.key() << std::dec
        << " ply=" << ss->ply << " found=1 mv=" << m1.to_usi_string() << std::endl;
} else {
    std::cerr << "YO_MATE1_QS key=" << std::hex << pos.key() << std::dec
        << " ply=" << ss->ply << " found=0 mv=none" << std::endl;
}
```

##### FEATURE_COUNT — 機能別発火回数カウント

特定機能の発火回数を RS/YO で比較し、差の有無を素早く確認する:

rshogi（alpha_beta.rs や qsearch.rs の該当箇所）:
```rust
eprintln!("RS_SMALL_PROBCUT");  // 発火のたびに1行出力
```

計測:
```bash
grep -ac "RS_SMALL_PROBCUT" /tmp/rs_stderr.txt   # 発火回数
grep -ac "YO_SMALL_PROBCUT" /tmp/yo_stderr.txt
```

**用途**: PLY drill-down の前に「そもそもこの機能の発火回数が違うのか」を O(1) で確認できる。
mate_1ply (QS/AB), small_probcut, futility, null_move 等に有効。

##### HISTORY — history スコア内訳（PLY{N}_MOVE に追加）

rshogi（undo_move 直後、PLY ログの直前）:
```rust
let dbg_pc = mv.moved_piece_after();
let dbg_to = mv.to();
let dbg_h = unsafe { ctx.history.as_ref_unchecked() };
let dbg_main = 2 * dbg_h.main_history.get(mover, mv) as i32;
let dbg_pawn = 2 * dbg_h.pawn_history.get(pos.pawn_history_index(), dbg_pc, dbg_to) as i32;
let dbg_ct = cont_history_tables(st, ctx, ply);
let mut dbg_cont = 0i32;
for (idx, _w) in [(0, 1), (1, 1), (2, 1), (3, 1), (5, 1)] {
    dbg_cont += dbg_ct[idx].get(dbg_pc, dbg_to) as i32;
}
let dbg_lph = 8 * dbg_h.low_ply_history.get(ply as usize, mv) as i32 / (1 + ply);
// → PLY ログに main={} pawn={} cont={} lph={} を追加
```

YO（undo_move 直後、PLY ログの直前）:
```cpp
Piece dbg_pc = pos.moved_piece(move);
Square dbg_to = move.to_sq();
int dbg_main = 2 * (int)mainHistory[us][move.raw()];
int dbg_pawn = 2 * (int)sharedHistory.pawn_entry(pos)[dbg_pc][dbg_to];
int dbg_cont = (int)(*contHist[0])[dbg_pc][dbg_to]
             + (int)(*contHist[1])[dbg_pc][dbg_to]
             + (int)(*contHist[2])[dbg_pc][dbg_to]
             + (int)(*contHist[3])[dbg_pc][dbg_to]
             + (int)(*contHist[5])[dbg_pc][dbg_to];
int dbg_lph = (ss->ply < LOW_PLY_HISTORY_SIZE)
    ? 8 * (int)lowPlyHistory[ss->ply][move.raw()] / (1 + ss->ply) : 0;
// → PLY ログに main=, pawn=, cont=, lph= を追加
```

#### 条件フィルタの書き方

**ply パス条件**（ply=3, path=4a3b/7g7f の場合）:

rshogi:
```rust
if ply == 3 && depth == {expected_depth}
    && st.nodes > {nodes_lo} && st.nodes < {nodes_hi}
    && st.stack[1].current_move.to_usi() == "4a3b"
    && st.stack[2].current_move.to_usi() == "7g7f"
{
    // ログ出力
}
```

YO:
```cpp
if (ss->ply == 3 && depth == {expected_depth}
    && nodes.load(std::memory_order_relaxed) > {nodes_lo}
    && nodes.load(std::memory_order_relaxed) < {nodes_hi}
    && (ss-2)->currentMove.to_usi_string() == "4a3b"
    && (ss-1)->currentMove.to_usi_string() == "7f7g")
{
    // ログ出力
}
```

**TT キー条件**（特定局面を追跡する場合）:

rshogi:
```rust
if pos.key() == 0x3134ab787c99c53d {
    eprintln!("RS_TARGET key={:016x} ply={} ...", pos.key(), ply);
}
```

YO:
```cpp
if (pos.key() == 0x3134ab787c99c53dULL) {
    std::cerr << "YO_TARGET key=" << std::hex << pos.key() << std::dec
        << " ply=" << ss->ply << " ..." << std::endl;
}
```

**ノード範囲の決め方**:
- ASP ログで各 iteration 開始時のノード数を確認
- 目的の iteration 開始ノード数を `nodes_lo`、次の iteration 直前を `nodes_hi` に設定
- 初回は広め（例: `> 0`）で取り、結果を見て絞り込む

**depth の計算**:
- `root depth - ply` が基本（例: root=14, ply=3 → depth=11 付近）
- IIR / SE で depth が変動するため、初回は depth 条件なしで取ることも有効

#### 挿入時の注意点

1. **rshogi の borrowing**: `cont_tables` を使う場合、借用衝突が起きやすい。`cont_history_tables()` を再呼び出しして新しいバインディングを作る
2. **rshogi の nodes_before_move**: ログ出力位置より前に `let nodes_before_move = st.nodes;` を挿入
3. **YO の ply1NodeCount**: `uint64_t(nodes)` を保存して差分計算に使用
4. **YO の出力**: `std::memory_order_relaxed` で `nodes.load()` する
5. **rshogi の Square 型**: `Display` trait 未実装。`eprintln!` では `{:?}` (Debug) を使用

### Phase 3: ビルド

```bash
# rshogi（並列ビルド可能な場合）
cargo build --release 2>&1 | tail -5

# YO（並列ビルド）
cd /mnt/nvme1/development/YaneuraOu/source && \
  make clean COMPILER=g++ > /dev/null 2>&1 && \
  make COMPILER=g++ -j$(nproc) 2>&1 | tail -5
```

ビルドエラーが出た場合:
- rshogi: `cargo build --release 2>&1 | grep "error\["` でエラー箇所を特定
- YO: `make ... 2>&1 | grep -a "error:"` でエラー箇所を特定
- 修正して再ビルド

### Phase 4: 計測実行

計測スクリプトを `/tmp/yo_measure_{tag}.sh` として生成・実行する。

#### スクリプトテンプレート（stderr 分離版）

```bash
#!/bin/bash
EVAL=/mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu/suisho5.bin
RS=/mnt/nvme1/development/rshogi/target/release/rshogi-usi
YO=/mnt/nvme1/development/YaneuraOu/source/YaneuraOu-by-gcc
HASH=${HASH:-256}
POS="{POSITION}"

run_engine() {
  local engine="$1"; local stderr_file="$2"; shift 2
  coproc ENG { "$engine" 2>"$stderr_file"; }; local pid=$ENG_PID
  echo "usi" >&${ENG[1]}; echo "setoption name USI_Hash value $HASH" >&${ENG[1]}
  for opt in "$@"; do echo "$opt" >&${ENG[1]}; done
  echo "setoption name EvalFile value $EVAL" >&${ENG[1]}; echo "isready" >&${ENG[1]}
  while read -r line <&${ENG[0]}; do [[ "$line" == readyok* ]] && break; done
  echo "usinewgame" >&${ENG[1]}; echo "position $POS" >&${ENG[1]}; echo "go depth {DEPTH}" >&${ENG[1]}
  while read -r line <&${ENG[0]}; do [[ "$line" == bestmove* ]] && break; done
  echo "quit" >&${ENG[1]}; wait "$pid" 2>/dev/null
}

echo "=== Running rshogi ==="
run_engine "$RS" /tmp/rs_{tag}_stderr.txt

echo "=== Running YO ==="
run_engine "$YO" /tmp/yo_{tag}_stderr.txt \
  "setoption name FV_SCALE value 24" "setoption name Threads value 1" "setoption name PvInterval value 0"

echo ""
echo "=== RS {LOG_TYPE} count ==="
grep -ac "RS_{LOG_TYPE}" /tmp/rs_{tag}_stderr.txt
echo "=== YO {LOG_TYPE} count ==="
grep -ac "YO_{LOG_TYPE}" /tmp/yo_{tag}_stderr.txt
```

`{POSITION}`, `{DEPTH}`, `{tag}`, `{LOG_TYPE}` は Phase 1 で確定した値に置換する。

**stderr 分離が重要**: `compare_nodes` ツールはノード数比較に特化しており、
デバッグログの取得には上記の coproc テンプレートを使用すること。

#### 実行

```bash
bash /tmp/yo_measure_{tag}.sh
```

### Phase 5: 結果比較

1. RS_ と YO_ の対応するログ行を手（mv=）でマッチング
2. 各フィールドを数値比較し、差分がある項目を特定
3. 差分テーブルを生成

#### 比較テーブル形式

```
| mv   | field  | RS     | YO     | diff   |
|------|--------|--------|--------|--------|
| 8c8d | main   | 5892   | 5892   | 0      |
| 8c8d | cont   | -14579 | -12839 | -1740  | ← 乖離
| 1c1d | main   | 5716   | 72     | +5644  | ← 大乖離
```

### Phase 6: 結果の解釈と次のステップ提案

比較結果を分析し、以下を報告:

1. **乖離の最大寄与因子**: どのフィールド（main/pawn/cont/lph/TT/eval 等）が最も乖離に寄与しているか
2. **乖離の発生タイミング**: どのノード数（≒どの aspiration iteration）で乖離が始まるか
3. **推定される原因カテゴリ**:
   - history 更新ロジックの差異
   - TT 書き込み/読み取りの差異
   - eval / correction の差異
   - 枝刈り条件の差異
   - 手順（move ordering）の差異
   - mate_1ply の偽陽性/偽陰性
   - pinned_pieces / blockers 計算の差異
4. **次の計測提案**: より深い ply や異なるログ種別で追跡すべきポイント

### Phase 7: yo_alignment_status.md の更新

計測で新たに判明した事実を `docs/performance/yo_alignment_status.md` に追記する。
推測ではなく計測事実のみを記録すること。

## ドリルダウンの定石

乖離を追い詰める際の典型的なドリルダウン順序:

```
1. cargo run --release -p tools --bin compare_nodes -- --sfen "..." --depth N で depth ごとのノード数比較 → 最初に乖離する depth N を特定
2. A/B テスト（後述）で乖離の原因機能を絞り込む（任意、効果的な場合のみ）
3. ROOT_MOVE cum 比較で乖離する aspiration window と root 手を同時特定
4. PLY1_MOVE で乖離する PLY1 の手を特定（cum 範囲を絞って）
5. PLY2_MOVE で乖離する PLY2 の手を特定
   ...（ply を掘り下げる）
6. 乖離が始まる最浅 ply でフィールド比較（ext, imp, ss, pv, r, nd, d）
7. 最初に異なるフィールドを特定し、そのフィールドの構成要素をブレークダウン
8. 差分フィールドに対応するコードパスを yo-compare で静的比較
9. 修正 → 再計測 → 確認
```

各段階で「差が 0 になる ply」と「差が出始める ply」の境界を明確にする。

### PLY ログが出ない場合のトラブルシュート

PLY{N}_MOVE ログを main moves loop に入れたのに出力されない場合:

1. **条件ミスを疑う前に、そのノードが main moves loop に到達するか確認する**
2. 以下の pre-loop パスで手が処理され早期リターンしている可能性がある:
   - **ProbCut (Step 12)**: キャプチャ手を独自ループで処理。cutoff すれば main loop に入らない
   - **Null Move Pruning (Step 10)**: cutoff すれば main loop に入らない
   - **TT cutoff (Step 5)**: 十分な深さの TT hit で即リターン
3. NODE ログ（ply 進入時）は出るが MOVE ログ（main loop 内）が出ない → pre-loop cutoff が原因
4. **対処**: ProbCut/NullMove 等の pre-loop パスにもログを追加して確認

### A/B テスト技法（機能トグル）

**深い depth で初めて乖離が出る場合**、PLY drill-down は探索木が巨大で非効率。
代わりに疑わしい機能を**両エンジンで同時に無効化**し、乖離が消えるかを確認する。

#### 手順

1. 疑わしい機能を RS/YO の両方で無効化（コードの該当ブロックを `if false` / `if (0)` でスキップ）
2. 両エンジンをビルド
3. `cargo run --release -p tools --bin compare_nodes -- --sfen "..." --depth N` で乖離 depth まで比較
4. 乖離が消えれば、その機能が根本原因（の経路上にある）

#### 推奨: 2x2 マトリクスで実行する

単発の「両方無効化」だけで結論しない。最低限、以下の 4 ケースを同じ条件で回す:

| ケース | RS | YO | 目的 |
|---|---|---|---|
| baseline | ON | ON | 現象の再現確認 |
| rs_only | OFF | ON | RS 側トグルの単独影響確認 |
| yo_only | ON | OFF | YO 側トグルの単独影響確認 |
| both_off | OFF | OFF | 原因経路の切り分け |

判定の目安:
- `both_off` だけ一致 → その機能は原因経路上にある可能性が高い
- `rs_only` / `yo_only` で探索木が大きく崩れる → トグルが侵襲的。shadow ログで再検証する

#### 非侵襲 shadow ログを優先する

機能を無効化すると探索木自体が変わるため、最初は「適用した場合の計算値だけをログ出力」して比較する。

例:
- `inc` の算出値を `applied_inc` として出す（実際の bestValue 更新はそのまま）
- `draw_jitter` の算出値を `jitter` として出す（return は変更しない）

shadow ログで差が確認できた場合のみ、機能トグルによる A/B に進む。

#### 代表的なトグル対象

| 機能 | RS 無効化 | YO 無効化 |
|------|----------|----------|
| Small ProbCut (Step 12) | 該当 `if` を `if false` | 該当 `if` を `if (0)` |
| mate_1ply (qsearch) | `pos.mate_1ply()` → `None` | `pos.mate1ply()` → `MOVE_NONE` |
| mate_1ply (alpha_beta) | 同上 | 同上 |
| Null Move Pruning | 該当ブロックスキップ | 該当ブロックスキップ |
| Singular Extension | SE ブロックスキップ | SE ブロックスキップ |
| Futility Pruning | 該当条件を常に偽に | 該当条件を常に偽に |
| draw_jitter (`value_draw`) | `RS_NO_DRAW_JITTER=1`（推奨） | `RS_NO_DRAW_JITTER=1`（推奨） |
| root tie-break `inc` | `RS_NO_INC=1`（推奨） | `RS_NO_INC=1`（推奨） |

#### 実例（2026-02-20 Small ProbCut の特定）

```
d=1〜d=18: 一致
d=19: -41711 差
d=20: -159030 差

A/B テスト: Small ProbCut を両エンジンで無効化
→ d=1〜d=20 全一致
→ Small ProbCut が原因経路上にあると確定

次の手順: Small ProbCut の発火回数カウント
→ RS=1747 vs YO=1402（345回の差）
→ TT に格納される値が異なることが原因

さらに drill-down: 特定キーで TT 値比較
→ mate_1ply の偽陽性が原因
→ pinned_pieces_excluding のバグを発見・修正
```

### FEATURE_COUNT による素早いトリアージ

A/B テストの前に、各機能の**発火回数**を RS/YO で比較するだけで原因候補を絞れる。

```bash
# 発火回数カウントの比較
echo "=== mate_1ply QS ==="
grep -ac "RS_MATE1_QS" /tmp/rs_stderr.txt
grep -ac "YO_MATE1_QS" /tmp/yo_stderr.txt

echo "=== mate_1ply AB ==="
grep -ac "RS_MATE1_AB" /tmp/rs_stderr.txt
grep -ac "YO_MATE1_AB" /tmp/yo_stderr.txt

echo "=== Small ProbCut ==="
grep -ac "RS_SMALL_PROBCUT" /tmp/rs_stderr.txt
grep -ac "YO_SMALL_PROBCUT" /tmp/yo_stderr.txt
```

カウントが一致すれば、その機能は原因ではない。差があれば深掘り対象。

### Step 2: ROOT_MOVE cum 比較（aspiration window + root 手の同時特定）

depth N の反復深化イテレーション中に出力される **全 ROOT_MOVE エントリ**（複数の
aspiration window を跨ぐ）の累積ノード数（cum）を RS/YO で比較し、最初に乖離する
エントリを特定する。これにより PLY drill-down の cum 範囲を大幅に絞り込める。

#### ログ形式

RS/YO 双方に `d=N` 限定で ROOT_MOVE + cum を出力:

rshogi（`alpha_beta.rs`、effort 加算直後）:
```rust
if self.state.root_depth == {N} {
    eprintln!(
        "RS_ROOT_MOVE d={} mv={} mc={} nd={} val={} cum={}",
        self.state.root_depth, mv.to_usi(), move_count, nodes_delta, value.raw(), self.state.nodes
    );
}
```

YO（`yaneuraou-search.cpp`、`rm.effort += nodes - nodeCount` 直後）:
```cpp
if (rootDepth == {N}) {
    std::cerr << "YO_ROOT_MOVE d=" << rootDepth
        << " mv=" << move.to_usi_string()
        << " mc=" << moveCount
        << " nd=" << (nodes.load(std::memory_order_relaxed) - nodeCount)
        << " val=" << (int)value
        << " cum=" << nodes.load(std::memory_order_relaxed) << std::endl;
}
```

#### 比較方法

```bash
# cum 値を抽出
grep -a "RS_ROOT_MOVE d=N" /tmp/rs_dN_stderr.txt | sed 's/.*cum=//' > /tmp/rs_cum.txt
grep -a "YO_ROOT_MOVE d=N" /tmp/yo_dN_stderr.txt | sed 's/.*cum=//' > /tmp/yo_cum.txt

# 最初の乖離行を検出
paste /tmp/rs_cum.txt /tmp/yo_cum.txt | awk '{d=$1-$2; if(d!=0) print NR, $1, $2, d}' | head -5
```

#### 結果の読み方

- **乖離行の直前まで cum 一致** → その区間の探索は完全同一
- **乖離行の mc=1** → 新しい aspiration window の開始。前 window の結果は同一
- **cum 差** から PLY drill-down の範囲を算出:
  `nodes_lo = 前行の cum`, `nodes_hi = 乖離行の cum（大きい方）`

### Step 2.5: 最初の不一致 1 点固定

ROOT/PLY ログは複数の depth/iteration が混在しやすい。次のキーで 1 ノードに固定してから深掘りする:

`root_d` + `iter` + `root move` + `ply` + `mc` + `key`

最低限の比較フィールド:
- `alpha`, `beta`
- `tt_hit`, `tt_move`, `tt_depth`, `tt_bound`, `tt_value`
- `static_eval` (`se`), `unadjusted_static_eval` (`use`), `correction_value` (`cv`)

`key` が一致した状態で差分が出るフィールドだけを次段で分解すること。

### r ブレークダウン（Step 7 の典型例）

r が異なる場合、各調整項を個別出力して原因の項を特定する:

```
r_base      = reduction() の戻り値
r_ttpv_pre  = ttPv ? 946 : 0          （SE 前）
r_ttpv_post = ttPv ? -(2618+...) : 0  （do_move 後）
r_base_add  = 843
r_mc        = -moveCount * 66
r_corr      = -abs(correctionValue) / 30450
r_cut       = cutNode ? 3094+... : 0
r_ttcap     = ttCapture ? 1415 : 0
r_cutoff    = cutoffCnt > 2 ? 1051+... : 0
r_ttmove    = mv == ttMove ? -2018 : 0
r_stat      = -statScore * 794 / 8192
```

### per-move ノード数の +1 アーティファクト

RS と YO でデバッグログ上の per-move ノード数が +1 ずれる:
- **RS**: `nodes_before_move` を do_move **前**に capture → do_move の +1 を含む
- **YO**: `plyNodeCount` を do_move **後**に capture → do_move の +1 を含まない

合計ノード数は一致するため、PLY ログ比較時は n/cum フィールドの ±1 差を無視してよい。

## TT カスケード伝播の理解

**TT 乖離は最も多い偽の手がかりを生む。** 以下のパターンを理解すること:

```
原因（例: mate_1ply バグ）
  → 特定局面で RS だけ mate スコアを TT に保存
  → その TT エントリが後の探索で参照される
  → TT cutoff / TT value の差 → bestValue の差
  → bestValue の差 → 枝刈り判定の差（step14, futility, null move 等）
  → 枝刈りの差 → 探索ノード数の差
  → ノード数の差 → history 更新量の差
  → history の差 → move ordering の差
  → move ordering の差 → さらなるノード数の差（カスケード）
```

**重要**: TT 値が異なる → TT ロジックにバグがある、と即断してはいけない。
TT は上流の探索結果を格納するだけ。「なぜその TT 値が格納されたか」を追跡する。

### TT 値の差を追跡する方法

1. 差が出る TT キーを特定（`pos.key()` を出力）
2. そのキーが最初に TT に書き込まれる箇所を特定（`tt_write` ログ追加）
3. 書き込み時の `value` の差 → その局面の探索結果の差
4. その局面の探索を drill-down → 真の根本原因に到達

## 確認済みサブシステム（再調査不要）

以下は RS/YO 間で完全一致が確認済み。乖離原因の候補から除外してよい:

| サブシステム | 確認内容 | 確認日 |
|-------------|---------|--------|
| TT 実装全般 | エントリサイズ(10B), クラスタ(32B/3エントリ), probe/save/replacement policy, generation 管理, relative_age | 2026-02-20 |
| first_entry (クラスタインデックス) | `mul_hi64(key, clusterCount)` + bit0手番 | 2026-02-20 |
| TT save 上書き条件 | 4条件(EXACT/キー不一致/深さPV優位/古い世代) + depth劣化 | 2026-02-20 |
| NNUE 評価値 | 同一局面で fresh NNUE eval が完全一致 | 2026-02-20 |
| ProbCut 手生成・ループ構造 | 逐次 next_move 方式に修正済み。バッファ collect は禁止 | 2026-02-21 |

**注意**: TT の「値が違う」のは TT ロジックのバグではなく、上流の探索結果の差。TT カスケード伝播を参照。

## 調査ショートカット（ノード差の方向から推定）

| RS vs YO | 意味 | 有力候補 |
|----------|------|---------|
| RS が多い (+) | RS がカットオフを見逃している | mate_1ply の偽陰性（RS が詰みを見つけられない）、pruning 条件の差 |
| RS が少ない (-) | RS が余分にカットオフしている | mate_1ply の偽陽性（RS が偽の詰みを検出）、TT eval/value の差 |

**再発パターン**: `mate_1ply` の差異 → TT に mate/通常スコアの差 → TT カスケード伝播 → 大規模ノード乖離。
mate_1ply は最初に A/B テストまたは FEATURE_COUNT で確認すべき。

## 過去の発見事例

### SE/tt_pv 順序バグ（2026-02-19 修正）

**症状**: r 値が正確に 946 (= lmr_ttpv_add) 異なる
**原因**: `r = reduction()` と `r += lmr_ttpv_add` が SE ブロックの後に配置されていた。SE の再帰 search_node が同一 ply の `tt_pv` を上書きし、post-SE の `tt_pv` で `r += 946` の適用判定をしていた
**修正**: r 計算を SE ブロック前に移動（YO 準拠: line 3253-3266）
**一般則**: YO のコード順序（reduction → ttPv調整 → lmrDepth → Step14 → SE → do_move）を忠実に守る。詳細は CLAUDE.md「SE と stack 上書きの注意」参照

### pinned_pieces_excluding バグ（2026-02-20 修正）

**症状**: d=19 で -41711、d=20 で -159030 のノード数差。Small ProbCut の発火回数が RS=1747 vs YO=1402 で不一致
**調査経路**:
1. A/B テスト: Small ProbCut 無効化 → 全一致 → Small ProbCut 経路が原因
2. Small ProbCut 発火カウント → RS が 345 回多い
3. TT キーで追跡 → 特定局面で TT に mate スコア vs 通常スコア
4. mate_1ply 発火比較 → RS=222 vs YO=201（QS）
5. 特定キーで mate_1ply 結果比較 → RS=found, YO=not found
6. DRAGON セクションのデバッグ → `pinned_pieces_excluding` が avoid 駒を pinner 候補から除外していない
**原因**: `pinned_pieces_excluding(them, avoid)` が `enemy_removed=Bitboard::EMPTY` を渡していた。
avoid 駒（移動元の竜）が pinner 候補に残存し、相手の合駒可能な駒を誤ってピンと判定
**修正**: `enemy_removed=avoid_bb` を渡す（YO 準拠: `pinners = (...) & avoid_bb & pieces(~C)`）
**教訓**:
- A/B テストは深い depth の乖離を効率的に切り分ける最強の初手
- TT カスケードの根本原因は TT ロジックではなく「TT に何を書き込んだか」
- mate_1ply のバグは TT 経由で全探索木に波及するため、影響が不釣り合いに大きい
- `pinned_pieces_excluding` のような低レベルヘルパーは、position.h の YO 実装と行単位で比較すべき

### TT eval 信用バグ（2026-02-20 修正）

**症状**: d=28 で -12776 のノード数差（RS が少ない）。d=1〜d=27 は完全一致
**調査経路**:
1. ROOT_MOVE cum 比較で最初の乖離ポイント特定
2. PLY drill-down → main_history update #1738 で bonus 差
3. bonus 差 → evalDiff の curr_eval 差(5) → unadjusted_static_eval 差(188 vs 183)
4. correction_value 一致 → 差は TT-stored eval vs fresh NNUE eval
5. 両エンジンの TT クラスタを dump → 内容一致（type-1 collision: key16=0x461f に別局面 eval=188 が格納）
6. YO のコード確認 → `USE_LAZY_EVALUATE` 未定義時は常に `evaluate(pos)` で上書き
**原因**: RS は `ttHit && eval有効 && !pv_node` 時に TT eval を信用していた。YO は常に NNUE 再評価。type-1 collision で別局面の eval 値が混入
**修正**: `eval_helpers.rs:497-501` で `tt_ctx.data.eval` → `nnue_evaluate(st, pos)` に変更
**教訓**:
- TT eval と NNUE eval の不一致は type-1 collision（16bit key 衝突）で発生する
- YO は `USE_LAZY_EVALUATE` 未定義のため常に NNUE 再評価する設計
- 5 点の eval 差が correction → evalDiff → history → pruning とカスケードし 12776 ノード差に増幅される

### デバッグトレースが探索ロジックを破壊（2026-02-23 発見）

**症状**: d=10 で RS=9836 vs YO=10040 のノード数差。d=1〜d=9 は完全一致
**調査経路**:
1. HW（History Write）トレースを両エンジンに追加し、mainHistory 全書き込みを seq 番号付きで比較
2. seq=0〜242 完全一致、seq=243 で RS-only の BEST 書き込み（key=52e3205db7ab76bb, p=11, d=6, mv=4h3g）
3. NK_ENTRY トレースで TT 状態を比較 → entry 時点で RS/YO 完全一致
4. NK_MV トレースで mc=1 の探索値を比較 → **両方とも val=125 beta=125 でカットオフ**
5. カットオフしているのに YO だけ BEST HW が出ない → update_all_stats が呼ばれていない
6. コード確認 → YO に挿入した `YO_NODE_END` トレース用の `if` 文が `if (!moveCount) ... else if (bestMove)` チェインを壊していた
**原因**: YO の `if (!moveCount)` と `else if (bestMove)` の間に `if (ply + depth == 17)` というトレース用 if を挿入。p=11, d=6 で条件が true になり、`else if (bestMove)` ブランチに入れず、`update_all_stats` がスキップされた
**修正**: トレースコードを if-else チェインの外に配置
**教訓**:
- **デバッグコード自体がバグの原因になる**。特に C/C++ の `if-else if` チェインは `{}` の欠如と相まって脆い
- 「乖離が解消された」と思ったら、**同一条件でクリーンビルド（git stash 後）でも一致するか必ず確認**すべき
- d=10 レベルの浅い depth での乖離がデバッグコード由来だった場合、**深い depth にはまだ本来の乖離が残っている**（この局面では d=22 で -49927 の実乖離あり）
- 調査専用のトレースコードは **最小限の `fprintf` + `return` なし** に限定し、制御フローに影響を与えない場所に配置する

### ProbCut バッファ collect バグ（2026-02-21 修正）

**症状**: d=13 で -20829 のノード数差（RS=215173, YO=236002）。d=1〜d=12 は完全一致
**調査経路**:
1. ROOT_MOVE → PLY drill-down で p=5 d=8 まで掘り下げ
2. p=5 の main moves loop でログが出ない → ProbCut (Step 12) がキャプチャを処理して早期リターン
3. ProbCut キャプチャ順序比較: RS=8i9g 先、YO=9d9g 先（同一局面、同一 key）
4. captureHistory スコア比較: RS で ch=5981、YO で ch=1525（差=4456）
5. RS は MovePicker の全手をバッファに事前 collect → TT手探索**前**にスコアリング
6. YO は逐次 next_move → TT手探索**後**にスコアリング
7. TT手(9d3d)の探索(33ノード)で captureHistory が更新され、スコアリング時点の値が異なる
**原因**: RS の `try_probcut` が MovePicker の全手をバッファに collect してから for ループでイテレートしていた。この構造では ProbCutInit（captures 生成 + score_captures + sort）が TT手の do_move/search/undo_move より**前**に実行される。TT手の探索で captureHistory が更新されるため、バッファ内のスコア順が YO と異なる
**修正**: `pruning.rs` のバッファ collect を `loop { mp.next_move() → filter → do_move → search → undo_move }` の逐次方式に変更
**教訓**:
- **バッファ collect は「スコアリング時点を固定する」副作用がある**。MovePicker を逐次呼び出す YO 方式が正しい
- 静的コード比較（yo-compare）では「同一式、異なるタイミング」のバグは発見不可能。計測データの差分が必須
- PLY drill-down で main moves loop にログが出ない場合、**ProbCut/NullMove 等の pre-loop パス**を即座に確認すべき
- PLY drill-down が p>=4 まで深い場合、A/B テスト（ProbCut 無効化）を先に試す方が効率的だった可能性あり

## エンジン起動方法（coproc 必須）

### pipe / file redirect は使用禁止

エンジンにコマンドを送る方法は **bash coproc** 一択。
他の方法は致命的な問題がある:

| 方法 | 問題 |
|------|------|
| `echo "..." \| ./engine 2>file` | pipe のバッファリングで stdin が一括到着。stderr/stdout が混在する環境あり |
| `./engine < input.txt 2>file` | ファイル入力は即座に EOF を送り、YO が `bestmove` 前に終了する（depth=1 で打ち切り） |
| **`coproc ENG { ./engine 2>file; }`** | **正解。コマンドを逐次送信し、`bestmove` を待ってから `quit` を送れる** |

### coproc テンプレート

```bash
run_engine() {
  local engine="$1" label="$2" errfile="$3"
  shift 3
  coproc ENG { "$engine" 2>"$errfile"; }
  local pid=$ENG_PID
  echo "usi" >&${ENG[1]}
  echo "setoption name USI_Hash value $HASH" >&${ENG[1]}
  for opt in "$@"; do echo "$opt" >&${ENG[1]}; done
  echo "isready" >&${ENG[1]}
  while read -r line <&${ENG[0]}; do [[ "$line" == readyok* ]] && break; done
  echo "usinewgame" >&${ENG[1]}
  echo "position sfen $SFEN" >&${ENG[1]}
  echo "go depth $DEPTH" >&${ENG[1]}
  while read -r line <&${ENG[0]}; do [[ "$line" == bestmove* ]] && break; done
  echo "$label done"
  echo "quit" >&${ENG[1]}
  wait "$pid" 2>/dev/null
}
```

### YO 固有のオプション注意

- YO は `EvalFile` を認識しない。`EvalDir` でディレクトリを指定するか、symlink `eval/nn.bin` を利用
- `FV_SCALE=24`, `Threads=1`, `PvInterval=0` が YO 側の標準オプション

## 環境変数ゲート型トレースの設計指針

### LazyLock パターン（RS 推奨）

ホットパスに挿入するトレースは `std::sync::LazyLock` で初回のみ環境変数を読む:

```rust
{
    use std::sync::LazyLock;
    static TARGET_KEY: LazyLock<Option<u64>> = LazyLock::new(|| {
        std::env::var("RS_NODE_KEY").ok().and_then(|s| u64::from_str_radix(&s, 16).ok())
    });
    if let Some(target) = *TARGET_KEY && pos.key() == target {
        eprintln!("...");
    }
}
```

環境変数が未設定なら **初回の Option::None 判定だけ** で以降ゼロコスト。

### getenv パターン（YO）

YO 側は `getenv()` が毎回呼ばれるため、**ホットパスでの性能影響に注意**:

```cpp
const char* nk_str = getenv("RS_NODE_KEY");
if (nk_str) {
    uint64_t target = strtoull(nk_str, nullptr, 16);
    if ((uint64_t)pos.key() == target) {
        fprintf(stderr, "...\n");
    }
}
```

大量トレース（HW_ALL 等）は static 変数でキャッシュする:

```cpp
static int hw_rd = -1;
static bool hw_init = false;
if (!hw_init) { hw_init = true; const char* e = getenv("RS_HW_RD"); if (e) hw_rd = atoi(e); }
if (hw_rd < 0 || current_root_depth != hw_rd) return;
```

### デバッグコードで探索ロジックを壊さない

**最重要ルール: if-else チェインの途中にトレースコードの `if` を挿入してはならない。**

```cpp
// ❌ 危険: トレースの if が else if チェインを壊す
if (!moveCount)
    bestValue = ...;

if (trace_condition) {            // ← 新しい if
    fprintf(stderr, "...\n");
}
else if (bestMove) {              // ← これは trace_condition の else になる！
    update_all_stats(...);        //    moveCount > 0 && trace_condition のとき呼ばれない
}

// ✅ 安全: トレースはチェインの外に配置
if (!moveCount)
    bestValue = ...;

if (trace_condition) {            // ← 独立した if（else なし）
    fprintf(stderr, "...\n");
}

if (!moveCount)
    ;  // already handled
else if (bestMove) {              // ← 正しく分岐する
    update_all_stats(...);
}
```

**実例（2026-02-23 の失敗）**: `YO_NODE_END` トレースを `if (!moveCount)` と `else if (bestMove)` の間に挿入し、`update_all_stats` が呼ばれないケースを作った。d=10 の乖離が「解消された」と誤認し、30+ セッションを浪費。

### 計測時はデバッグコードを除去する

`compare_nodes` 等で大量局面を処理する場合、デバッグコードの有無で探索速度が変わる。
特に YO の `getenv()` はホットパスで顕著。計測前に `git stash` でクリーンにすること。

## 調査方向の検証

PLY drill-down やコード条件の比較中に、調査の方向性に迷った場合（同じ場所を
堂々巡りしている、条件を網羅的に潰しているが成果が出ない等）、
**yo-review スキルを Task サブエージェントとして呼び出す**こと。

```
Task(subagent_type="general-purpose", prompt="以下の調査ログをレビューして、
方向性が正しいか判断してください。yo-review スキル（.claude/skills/yo-review/SKILL.md）
を読んでから回答してください。\n\n{調査ログ}")
```

yo-review は計測手順には関与せず、戦略的な軌道修正のみを行う。

## 既存デバッグコードの扱い

- デバッグログは作業完了後に**必ず除去**すること
- `git checkout -- {file}` で元に戻すのが最も安全
- コメントアウトして残すのは禁止（次回セッションの混乱の元）
- デバッグコードはコミットしない（作業ブランチのワーキングツリーに留める）

### 実験後クリーンアップチェック

- [ ] `RS_` / `YO_` プレフィックスの一時ログが残っていない
- [ ] `RS_NO_*` / `RS_DISABLE_*` などの一時 env 分岐が残っていない
- [ ] A/B 用の `if false` / `if (0)` が残っていない
- [ ] `/tmp/*.sh` の実験スクリプトに依存する手順が本体コードに残っていない

### 再利用テンプレート（A/B マトリクス）

```bash
# 同一局面・同一深さで 4 ケースを実行する雛形
run_case baseline ""
run_case rs_only "RS_NO_TARGET=1"                # RS のみトグル
run_case yo_only "" "RS_NO_TARGET=1"             # YO のみトグル
run_case both_off "RS_NO_TARGET=1" "RS_NO_TARGET=1"
```

`run_case` は stderr を個別ファイルに分離し、最後に `compare_nodes` 結果と
`RS_*/YO_*` ログ行数を併記する形にする。
