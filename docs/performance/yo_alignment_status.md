# YaneuraOu ノード数一致調査 ステータス

最終更新: 2026-02-18
コミット: d0fa6e40 (`fix-search-tt-6i7h` ブランチ)

## 調査ゴール

**YaneuraOu の探索木とノード数を全深度で 100% 一致させること。**

乖離率が小さくても許容しない。差が 1 ノードでも修正対象。
100% 一致達成後に独自チューニングを実施する。

---

## 計測条件

- `cargo clean && cargo build --release` でフルビルド（incremental cache 破損を防止）
- USI_Hash=1, Threads=1（両エンジン共通）
- FV_SCALE=24（YO 側のみ必要。rshogi 側は NNUE ロード時に内部設定）
- **EvalFile 必須**: rshogi は `setoption name EvalFile value /path/to/suisho5.bin` を明示設定（未設定では NNUE 未使用で評価が大幅に異なる）
- **PvInterval=0 必須**: YO 側は `setoption name PvInterval value 0` を設定（デフォルト 300ms では浅い深度の出力が省略される）
- rshogi バイナリ: `/mnt/nvme1/development/rshogi/target/release/rshogi-usi`
- YO バイナリ: `/mnt/nvme1/development/YaneuraOu/source/YaneuraOu-by-gcc`
- NNUE: `/mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu/suisho5.bin`

---

## 標準計測コマンド

### 基本比較スクリプト（再利用可能）

```bash
# startpos d1-d8 比較（約 15 秒×2）
bash /tmp/startpos_compare.sh

# pos1 d1-d8 比較（約 20 秒×2）
bash /tmp/pos1_compare.sh
```

スクリプト内容（随時 /tmp に再生成して使用）:

**startpos_compare.sh:**
```bash
#!/bin/bash
EVAL=/mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu/suisho5.bin
RS=/mnt/nvme1/development/rshogi/target/release/rshogi-usi
YO=/mnt/nvme1/development/YaneuraOu/source/YaneuraOu-by-gcc

echo "=== rshogi startpos d1-d8 ==="
{ echo "usi"; echo "setoption name USI_Hash value 1"; echo "setoption name EvalFile value $EVAL"
  echo "isready"; echo "usinewgame"; echo "position startpos"; echo "go depth 8"; sleep 15; echo "quit"
} | $RS 2>/dev/null | grep "^info depth"

echo ""
echo "=== YO startpos d1-d8 ==="
{ echo "usi"; echo "setoption name USI_Hash value 1"; echo "setoption name FV_SCALE value 24"
  echo "setoption name Threads value 1"; echo "setoption name PvInterval value 0"
  echo "setoption name EvalFile value $EVAL"; echo "isready"; echo "usinewgame"
  echo "position startpos"; echo "go depth 8"; sleep 15; echo "quit"
} | $YO 2>/dev/null | grep -a "^info depth"
```

**pos1_compare.sh:**
```bash
#!/bin/bash
SFEN="sfen +B1sgk1snl/6gb1/p3pp1pp/1pr3p2/3NP4/2p4P1/PP1P1PP1P/2G2S1R1/L3KG1NL w NLPsp 32"
EVAL=/mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu/suisho5.bin
RS=/mnt/nvme1/development/rshogi/target/release/rshogi-usi
YO=/mnt/nvme1/development/YaneuraOu/source/YaneuraOu-by-gcc

echo "=== rshogi pos1 d1-d8 ==="
{ echo "usi"; echo "setoption name USI_Hash value 1"; echo "setoption name EvalFile value $EVAL"
  echo "isready"; echo "usinewgame"; echo "position $SFEN"; echo "go depth 8"; sleep 20; echo "quit"
} | $RS 2>/dev/null | grep "^info depth"

echo ""
echo "=== YO pos1 d1-d8 ==="
{ echo "usi"; echo "setoption name USI_Hash value 1"; echo "setoption name FV_SCALE value 24"
  echo "setoption name Threads value 1"; echo "setoption name PvInterval value 0"
  echo "setoption name EvalFile value $EVAL"; echo "isready"; echo "usinewgame"
  echo "position $SFEN"; echo "go depth 8"; sleep 20; echo "quit"
} | $YO 2>/dev/null | grep -a "^info depth"
```

### 任意局面・任意深度の 1 コマンド比較テンプレート

```bash
EVAL=/mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu/suisho5.bin
RS=/mnt/nvme1/development/rshogi/target/release/rshogi-usi
YO=/mnt/nvme1/development/YaneuraOu/source/YaneuraOu-by-gcc
POS="startpos"          # または "sfen ..."
DEPTH=10

{ echo "usi"; echo "setoption name USI_Hash value 1"; echo "setoption name EvalFile value $EVAL"
  echo "isready"; echo "usinewgame"; echo "position $POS"
  echo "go depth $DEPTH"; sleep 30; echo "quit"
} | $RS 2>/dev/null | grep "^info depth $DEPTH "

{ echo "usi"; echo "setoption name USI_Hash value 1"; echo "setoption name FV_SCALE value 24"
  echo "setoption name Threads value 1"; echo "setoption name PvInterval value 0"
  echo "setoption name EvalFile value $EVAL"; echo "isready"; echo "usinewgame"
  echo "position $POS"; echo "go depth $DEPTH"; sleep 30; echo "quit"
} | $YO 2>/dev/null | grep -a "^info depth $DEPTH "
```

### YO ビルド（必ず逐次ビルド）

```bash
cd /mnt/nvme1/development/YaneuraOu/source
make clean COMPILER=g++ && make COMPILER=g++
# NOTE: -j$(nproc) はリンカ競合で失敗することがあるので並列フラグなし
```

---

## ノード数一致状況（最終更新: 2026-02-18）

### startpos

| depth | rshogi | YO | diff |
|-------|--------|-------|------|
| d1  | 30    | 30    | 0 |
| d2  | 604   | 604   | 0 |
| d3  | 651   | 651   | 0 |
| d4  | 1020  | 1020  | 0 |
| d5  | 1434  | 1434  | 0 |
| d6  | 1601  | 1601  | 0 |
| d7  | 2253  | 2253  | 0 |
| d8  | 4024  | 4024  | 0 |
| d9  | 5017  | 5017  | 0 |
| d10 | 5210  | 5210  | 0 |
| d11 | 12816 | 12816 | 0 |
| d12 | 24941 | 24941 | 0 |
| d13 | 30656 | 30656 | 0 |
| **d14** | **59047** | **60128** | **-1081** ← 未解決 |

### line11818 (`position startpos moves 2g2f 8c8d 2f2e`)

| depth | rshogi | YO | diff |
|-------|--------|-------|------|
| d1  | 31    | 31    | 0 |
| d2  | 151   | 151   | 0 |
| d3  | 268   | 268   | 0 |
| d4  | 356   | 356   | 0 |
| d5  | 949   | 949   | 0 |
| d6  | 994   | 994   | 0 |
| d7  | 1954  | 1954  | 0 |
| d8  | 3049  | 3049  | 0 |
| d9  | 10555 | 10555 | 0 |
| d10 | 11604 | 11604 | 0 |
| d11 | 31460 | 31460 | 0 |
| d12 | 46418 | 46418 | 0 |
| **d13** | **87181** | **90923** | **-3742** ← 未解決 |
| **d14** | **113266** | **103606** | **+9660** ← 未解決（符号反転） |

### pos1 (`sfen +B1sgk1snl/6gb1/p3pp1pp/1pr3p2/3NP4/2p4P1/PP1P1PP1P/2G2S1R1/L3KG1NL w NLPsp 32`)

| depth | rshogi | YO | diff (nodes) | cp 一致 |
|-------|--------|----|--------------|---------|
| d1 | 92   | 92   | 0 | ✅ 228cp |
| d2 | 181  | 181  | 0 | ✅ 252cp |
| d3 | 385  | 385  | 0 | ✅ 235cp |
| d4 | 608  | 608  | 0 | ✅ 272cp |
| d5 | 756  | 756  | 0 | ✅ 363cp |
| d6 | 1364 | 1364 | 0 | ✅ 362cp |
| d7 | 1531 | 1531 | 0 | ✅ 366cp |
| **d8** | **4571** | **4624** | **-53** ← 調査中 | ✅ 348cp |
| **d9** | **5565 (324cp)** | **28161 (100cp)** | **-22596** ← 未解決 | ❌ |

---

## 未解決乖離の詳細

### pos1 d8: -53 ノード（調査継続中）

#### 調査進捗

乖離の発生経路は特定済み。根本原因（iter=3 での PLY3 branch 差）の原因が未特定。

**確認済み一致項目:**
- アスピレーションウィンドウ（5 イテレーション全て同一）
- LMR パラメータ（全定数一致）
- Pruning パラメータ（全定数一致）
- TT 置換ポリシー（完全一致）
- Correction history 係数（完全一致）
- Singular Extension 条件コード（完全一致）

**アスピレーションウィンドウ（両エンジン一致）:**

| iter | adjusted_depth | alpha | beta | result |
|------|---------------|-------|------|--------|
| 1 | 8 | 300 | 332 | fail-low |
| 2 | 8 | 284 | 300 | fail-low |
| 3 | 8 | 263 | 284 | fail-high (7f7g+) |
| 4 | 7 | 263 | 312 | fail-high (7f7g+) |
| 5 | 6 | 275 | 349 | success |

**ルート手ノード数（iter=4 に 42 ノード差）:**

| iter | 手 | rshogi | YO | 差 |
|------|---|--------|-----|-----|
| 3 | 7f7g+ | 1151 | 1151 | 0 |
| **4** | **7f7g+** | **402** | **444** | **-42** |
| 5 | 7f7g+ | 38 | 47 | -9 |
| 合計 | | 4571 | 4624 | -53 |

**乖離の伝播経路（特定済み）:**

```
iter=3: PLY3 (7g7h後) の TT bound が異なる
  rshogi: Bound::Upper, tt_move=none  （fail-low で終了）
  YO:     Bound::Lower, tt_move=あり  （fail-high で終了）
    ↓
iter=4: PLY3 での Singular Extension 判定
  rshogi: depth=9 の TT が Bound::Upper かつ tt_move=none → SE 不発動
  YO:     Bound::Lower + tt_move → SE 発動
    ↓
YO のみ SE 除外探索で PLY4 (N*6c) を呼び出す
  → PLY4 fail-low → tt_pv |= parent_tt_pv(=true) → TT に is_pv=true で保存
    ↓
本探索の PLY4 が TT 参照 → tt_pv=true（YO）vs tt_pv=false（rshogi）
    ↓
PLY5 の探索 depth が異なる → rshogi: depth=8, YO: depth=9+
    ↓
ノード差 -42（iter=4）、累計 -53
```

**根本原因（未解決）:**

iter=3 で PLY3 が fail-low（rshogi）vs fail-high（YO）になる原因が未特定。
iter=3 での root 手 7f7g+ のノード数は一致（1151）しているが、
PLY3 内部の探索分布が異なり、結果として異なる bound で TT に保存される。

→ **次の調査**: iter=3 での PLY3 のノード数・TT 手・評価値をログで比較する

#### 関連コミット

- `d0fa6e40`: SE 後の `depth++` が reduction/LMP/step14 に影響しないよう `original_depth` を使用（YO 準拠）

---

### pos1 d9: 大幅乖離（未調査）

| | rshogi | YO |
|--|--------|-----|
| nodes | 5565 | 28161 |
| cp | 324cp | 100cp |
| seldepth | 16 | 21 |
| PV | `7f7g+ 9a7c 7d7c 6e7c 7g7h ...` | `7f7g+ 9a7c 7d7c 6e7c 6a6b 7h7g ...` |

PV は 4 手目 `6e7c` まで一致し 5 手目で分岐。YO の seldepth=21 は rshogi=16 より深く、
YO がより深い反証を発見して 100cp に修正していると推定。
rshogi が d9 で 324cp と過大評価している原因を要調査。

---

### startpos d14: -1081 ノード（未調査）

- 乖離率: 1.8%
- d1-d13 は完全一致
- 調査未着手

---

### line11818 d13/d14: 乖離（未調査）

- d13: -3742（4.1%）
- d14: +9660（9.3%、符号反転）
- d13/d14 の符号反転は探索パスのカスケード分岐を示唆
- `mate_1ply` の差分ではない（can_king_escape 修正後も変化なし）
- 調査未着手

---

## 修正済み一覧

| コミット | 内容 | 影響 |
|---------|------|------|
| `07034495` | 各手ごとの sel_depth リセット削除 | ノード数一致改善 |
| `124fff7d` | `can_king_escape_with_from` の王キャプチャ逃げ処理を YO 準拠に修正 | d8 cp 修正（296→348） |
| `d0fa6e40` | SE 後 `depth++` が reduction/LMP/step14 に影響しないよう `original_depth` を使用 | YO 準拠アライメント |

---

## 計測時の注意事項

1. **cargo incremental cache 破損**: `git checkout` で異なるコミットを行き来すると壊れる。信頼性の高い結果が必要な場合は `cargo clean && cargo build --release` を使用
2. **YO FV_SCALE**: 必ず `setoption name FV_SCALE value 24` を設定（デフォルト 16 だと eval が異なる）
3. **YO 出力のバイナリ文字**: `grep -a` を使用
4. **root move ごとのノード数**: rshogi は do_move 前に +1 するため、root move 単位では YO と +1 ずれる（depth 全体の合計は一致）
5. **YO ビルドの並列化**: `-j$(nproc)` はリンカ競合で失敗することがある。並列フラグなしでビルド

---

## 次の調査ステップ（優先順）

### 1. pos1 d8: iter=3 PLY3 bound 差の根本原因（優先度高）

iter=3 で 7f7g+ を探索中に PLY3 の結果が異なる原因を調査。

手順:
1. iter=3 限定の PLY3 ノード数ログを追加（rshogi / YO 両方）
2. PLY3 に入る時点の TT 状態（iter=2 の TT 書き込み結果）を比較
3. PLY3 内で探索する手の順序・枝刈り発動を比較
4. 差が見つかったコードパスを `yo-compare` スキルで確認

### 2. pos1 d9: 大幅乖離調査（優先度高）

1. d9 で PV が分岐する 5 手目の原因を調査
2. rshogi の seldepth=16 が YO の 21 より浅い原因を特定
3. 4 手後局面 (`7f7g+ 9a7c 7d7c 6e7c`) から d5-d6 で再比較し、早期乖離を確認

### 3. startpos d14 / line11818 d13-d14（優先度中）

1. 乖離が最初に出る depth を特定（startpos: d14、line11818: d13）
2. 乖離 depth で root move 別ノード数を比較し、分岐起点の手を絞る
3. 分岐起点の手を含む局面で低 depth 比較（乖離が早期に出る局面を探す）
4. 差が見つかったコードパスを `yo-compare` スキルで確認
