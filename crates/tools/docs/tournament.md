# tournament — 並列トーナメント・SPRT 検定

複数エンジン間の総当たり (round-robin) 対局、base-vs-N 対局、または SPRT（逐次確率比検定）を並列実行するツール。
対局ログは analyze_selfplay 互換の JSONL 形式で出力される。

## ビルド

```bash
cargo build -p tools --bin tournament --release
```

## モード

| モード | 概要 | 主なオプション |
|--------|------|---------------|
| **総当たり** | N エンジン間の全 C(N,2) ペアを対局 | `--engine` × N |
| **base-vs-N** | 基準 1 体 vs 他 N 体のみ対局 | `--base-label` |
| **SPRT** | A/B 2 エンジンで有意差を逐次判定、境界到達で自動停止 | `--sprt` |

## クイックスタート

### 総当たり（2 エンジン、各方向 100 局 = 合計 200 局）

```bash
./target/release/tournament \
  --engine target/release/rshogi-usi-v1 --engine-label v1 \
  --engine target/release/rshogi-usi-v2 --engine-label v2 \
  --games 100 --byoyomi 1000 --hash-mb 256 --threads 1 --concurrency 8 \
  --startpos-file data/startpos/start_sfens_ply32.txt \
  --out-dir runs/selfplay/$(date +%Y%m%d_%H%M%S)-v1-vs-v2
```

### SPRT（有意差の早期判定）

test エンジンが base より +5 nelo 以上強いかを有意水準 95% で検定する。
差が明確なら早期に判定終了し、差が微妙なら `--games` 上限まで対局を続ける。

- `--sprt-nelo0 0`: H0 = 差なし（帰無仮説）
- `--sprt-nelo1 5`: H1 = +5 nelo 以上の差あり（対立仮説、検出したい最小効果量）
- `--sprt-alpha 0.05`: H0 が真なのに H1 と誤判定する確率の上限 5%
- `--sprt-beta 0.05`: H1 が真なのに H0 と誤判定する確率の上限 5%
- `--games 5000`: SPRT が境界に達しなかった場合の対局数上限（保険）

`--sprt-base-label` を省略すると `--base-label` の値が自動で使われる。

```bash
./target/release/tournament \
  --engine /path/to/base-engine --engine-label base \
  --engine /path/to/test-engine --engine-label test \
  --games 5000 --byoyomi 1000 --hash-mb 256 --threads 1 --concurrency 16 \
  --startpos-file data/startpos/start_sfens_ply32.txt \
  --base-label base \
  --sprt --sprt-test-label test \
  --sprt-nelo0 0 --sprt-nelo1 5 --sprt-alpha 0.05 --sprt-beta 0.05 \
  --out-dir runs/selfplay/$(date +%Y%m%d_%H%M%S)-sprt-base-vs-test
```

## CLI オプション

### 対局制御

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--engine PATH` | (必須、2 つ以上) | エンジンバイナリパス。`--engine` の数だけエンジン登録される |
| `--engine-label LABEL` | パスから自動生成 | エンジンラベル（`--engine` と同数・同順で指定）。同一パスを複数回指定する場合は区別のため必須 |
| `--games N` | 100 | 各方向の対局数（双方向で 2×N 局/ペア） |
| `--max-moves N` | 512 | 1 局の最大手数（超過で引き分け） |
| `--concurrency N` | 1 | 並列対局数。1 対局は手番制で約 1 CPU スレッド消費 |
| `--report-interval N` | 10 | N 局ごとに進捗を表示 |

### 時間・エンジン設定

時間管理オプション (`--byoyomi`, `--btime`/`--binc`, `--depth`, `--nodes`) のいずれかの指定が必須。
すべて省略するとエラーになる。

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--byoyomi MS` | — | 秒読み（ミリ秒）。`--btime`/`--binc` と排他 |
| `--btime MS` | 0 | 持ち時間（ミリ秒、フィッシャー時計）。`--byoyomi` と排他 |
| `--binc MS` | 0 | 1 手ごとの加算時間（ミリ秒、フィッシャー時計） |
| `--depth N` | — | 深さ制限（`go depth N` を送出） |
| `--nodes N` | — | ノード制限（`go nodes N` を送出） |
| `--threads N` | 1 | Threads USI オプション |
| `--hash-mb N` | 256 | USI_Hash（MiB） |
| `--usi-option "Name=Value"` | — | 全エンジン共通の USI オプション（複数指定可） |
| `--engine-usi-option "IDX:Name=Value"` | — | エンジン個別の USI オプション（0 始まりインデックス）。共通 `--usi-option` にマージされ、同じキーは engine 個別指定が上書きする |
| `--strict-engine-usi-option` | false | `--engine-usi-option` を指定したエンジンでは共通 `--usi-option` を完全に置換する（旧挙動） |
| `--engine-params-file "IDX:FILE"` | — | SPSA `.params` ファイルから USI オプションを読み込む。`--engine-usi-option` と併用可（マージ） |

### 開始局面

| オプション | 説明 |
|-----------|------|
| `--startpos-file FILE` | 開始局面ファイル（1 行 1 局面、USI position 形式）。省略時は平手初期局面 1 局面のみ使用（全対局が同一局面になるため棋力評価には不向き）。**棋力評価では必須** |

### 出力

| オプション | 説明 |
|-----------|------|
| `--out-dir DIR` | (必須) 出力ディレクトリ。`{label_i}-vs-{label_j}.jsonl` と `meta.json` が生成される |

### base-vs-N モード

| オプション | 説明 |
|-----------|------|
| `--base-label LABEL` | 基準エンジンのラベル。このエンジンと他全エンジンのペアのみ対局する |

### SPRT モード

| オプション | デフォルト | 説明 |
|-----------|-----------|------|
| `--sprt` | — | SPRT を有効化。境界到達で新規対局の供給を停止し、進行中ゲームを完了待ちで drain |
| `--sprt-test-label LABEL` | (必須) | H1 側（challenger）のエンジンラベル。正の nelo = このエンジンが強い |
| `--sprt-base-label LABEL` | `--base-label` | H0 側（base）のエンジンラベル。省略時は `--base-label` を流用 |
| `--sprt-nelo0 F` | 0.0 | H0 仮説の正規化 Elo（通常 0 = 差なし） |
| `--sprt-nelo1 F` | 5.0 | H1 仮説の正規化 Elo（検出したい最小効果量） |
| `--sprt-alpha F` | 0.05 | 第一種過誤率 α（H0 が真なのに H1 を採択する確率の上限） |
| `--sprt-beta F` | 0.05 | 第二種過誤率 β（H1 が真なのに H0 を採択する確率の上限） |
| `--sprt-report-interval N` | 10 | ペア何単位ごとに SPRT レポートを出力 |

#### SPRT 出力例

```
[SPRT pair=1014 | test vs base] LLR=+2.950 (bounds -2.94..+2.94)  nelo=+38.05 ± 15.12  penta=[184, 15, 508, 17, 290]  state=accept_h1
[SPRT] terminal decision reached; draining 15 in-flight game(s)...

=== SPRT Summary (test vs base) ===
bounds: LLR ∈ [-2.944, +2.944]  (alpha=0.05, beta=0.05)
nelo hypotheses: H0=+0.0  H1=+5.0
stopped_at:  pairs=1014, LLR=+2.950, decision=accept_h1
             nelo=+38.05 ± 15.12  penta=[184, 15, 508, 17, 290]
final:       pairs=1022, LLR=+2.889, decision=running
             nelo=+37.02 ± 15.06  penta=[187, 15, 512, 17, 291]
================================
```

`stopped_at` は境界到達時点のスナップショット。`final` は drain 完了後の最終値で、
進行中ゲームの結果を含むため LLR・ペア数が微差する。判定は `stopped_at` の時点で
確定済みのため、`final` の `decision` が `running` と表示されることがあるが、
判定結果が覆ることはない。

#### SPRT 用語

| 用語 | 意味 |
|------|------|
| **LLR** (Log-Likelihood Ratio) | 対数尤度比。H1 と H0 のどちらがデータをよく説明するかを示すスコア |
| **nelo** (Normalized Elo) | pentanomial ペアスコアの分散で正規化した Elo。引分率に依存しない棋力差推定 |
| **penta** (Pentanomial) | 2 局ペア（同一開始局面・先後入替）の結果を 5 カテゴリ [LL, LD, DD/WL, WD, WW] に分類した分布。中央の DD/WL は「両局引き分け」または「test の勝ち/負けが 1 局ずつ」でペアスコアが同値 (0.5) になるため同カテゴリ |
| **accept_h1** | test が base より `nelo1` 以上強い（有意差あり） |
| **accept_h0** | 差が `nelo0` 以下（有意差なし） |

### 外部エンジンとの対局

rshogi と YaneuraOu のようにエンジンごとに USI オプションが異なる場合、`--engine-usi-option` で個別指定する。
共通 `--usi-option` を完全に置換したい場合は `--strict-engine-usi-option` を併用する:

```bash
./target/release/tournament \
  --engine target/release/rshogi-usi --engine-label rshogi \
  --engine /path/to/YaneuraOu-binary --engine-label yaneuraou \
  --strict-engine-usi-option \
  --engine-usi-option "0:EvalFile=eval/model.bin" \
  --engine-usi-option "1:EvalDir=/path/to/eval_dir" \
  --engine-usi-option "1:FV_SCALE=28" \
  --engine-usi-option "1:BookFile=no_book" \
  --games 100 --byoyomi 1000 --concurrency 8 \
  --out-dir runs/selfplay/$(date +%Y%m%d_%H%M%S)-rshogi-vs-yo
```

## 結果集計 — analyze_selfplay

tournament の出力 JSONL を読み込み、勝率・Elo 差・NPS 等を集計する:

```bash
./target/release/analyze_selfplay runs/selfplay/{DIR}/*.jsonl
```

### 出力内容

- **エンジン別 勝敗**（先後合算・先後別勝率）
- **直接対決**（Elo 差 ± CI、nElo 差 ± CI）
  - Elo 差: trinomial（1 局ごとの WDL）ベース
  - nElo: pentanomial（ペア単位）ベース。開始局面・先後の交絡を除去した、より正確な推定
- **追加統計**（平均手数、先手勝率、NPS、depth、seldepth 等）

### SPRT post-hoc 判定

完了済みログから SPRT 判定を再現・再検討できる:

```bash
./target/release/analyze_selfplay \
  runs/selfplay/{DIR}/*.jsonl \
  --sprt --sprt-base-label base --sprt-test-label test \
  --sprt-nelo0 0 --sprt-nelo1 5
```

- `--sprt`: SPRT 判定モードを有効化
- `--sprt-base-label` / `--sprt-test-label`: pentanomial の集計方向を指定する（どちらが test 側か）。
  nelo の符号は test 視点で決まるため、**両方とも省略不可**。
  値は tournament 実行時の `--engine-label` で指定したラベルと一致させること
- `--sprt-nelo0` / `--sprt-nelo1`: tournament 実行時と異なる閾値を指定して
  「この閾値なら何局で打ち切れたか」を事後検証できる

通常の集計出力の末尾に SPRT レポートが追加される。
