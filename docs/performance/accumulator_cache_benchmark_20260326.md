# AccumulatorCaches (Finny Tables) + MAX_DEPTH チューニング ベンチマーク

日付: 2026-03-26

## 計測条件

- `go depth 20`, Threads=1, Hash=256MB
- 15局面（実対局棋譜から ply 帯別に抽出）
  - Pos 1-3: ~ply 20（序盤）
  - Pos 4-6: ~ply 40（中盤入口）
  - Pos 7-9: ~ply 60（中盤）
  - Pos 10-12: ~ply 80（終盤入口）
  - Pos 13-15: ~ply 100（終盤）
- 局面ソース: `runs/selfplay/20260325-v82_300-vs-aoba-fisher3m10s/0:v82-300-vs-1:AobaNNUE.jsonl`
- 注: NNUE 学習プロセスが同時実行中のため絶対値は参考。同一条件での相対比較は有効

## LayerStacks (v82-300, L1=1536)

EvalFile: `/mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin`
LS_BUCKET_MODE=progress8kpabs, FV_SCALE=28

| Pos | ply帯 | D=1 (no cache) | D=4 + cache | 変化 |
|-----|-------|---------------|-------------|------|
| 1 | ~20 | 537,085 | 565,274 | +5.2% |
| 2 | ~20 | 552,333 | 549,362 | -0.5% |
| 3 | ~20 | 510,672 | 573,125 | +12.2% |
| 4 | ~40 | 499,880 | 502,711 | +0.6% |
| 5 | ~40 | 497,753 | 515,538 | +3.6% |
| 6 | ~40 | 514,465 | 532,060 | +3.4% |
| 7 | ~60 | 495,255 | 525,189 | +6.0% |
| 8 | ~60 | 628,658 | 630,373 | +0.3% |
| 9 | ~60 | 600,866 | 605,037 | +0.7% |
| 10 | ~80 | 615,373 | 648,180 | +5.3% |
| 11 | ~80 | 629,824 | 636,778 | +1.1% |
| 12 | ~80 | 524,677 | 509,894 | -2.8% |
| 13 | ~100 | 586,171 | 583,732 | -0.4% |
| 14 | ~100 | 325,232 | 297,553 | -8.5% |
| 15 | ~100 | 344,103 | 331,590 | -3.6% |
| **平均** | | **524,156** | **533,760** | **+1.8%** |

注: この計測は AccumulatorCaches の before/after 比較としては不完全。before バイナリ (commit 683c3e5e) との比較では +43〜+60% の改善を確認済み（別途計測、PR #397 コメント参照）。ここでの D=1 は既に AccumulatorCaches が有効な状態での MAX_DEPTH 比較。

## HalfKA_hm (v63, L1=1024)

EvalFile: `eval/halfka_hm_1024x2-8-64_crelu/v63.bin`, FV_SCALE=14

| Pos | ply帯 | D=1 | D=2 | D=3 | D=4 | best |
|-----|-------|-----|-----|-----|-----|------|
| 1 | ~20 | 424,045 | **479,984** | 462,803 | 462,562 | D=2 (+13.2%) |
| 2 | ~20 | 425,222 | **462,101** | 459,427 | 455,735 | D=2 (+8.7%) |
| 3 | ~20 | 394,161 | **410,464** | 416,145 | 426,181 | D=4 (+8.1%) |
| 4 | ~40 | 334,527 | 320,040 | **351,165** | **397,338** | D=4 (+18.8%) |
| 5 | ~40 | 373,355 | **410,627** | 413,496 | 359,533 | D=3 (+10.8%) |
| 6 | ~40 | 351,242 | **398,571** | 330,657 | 349,038 | D=2 (+13.5%) |
| 7 | ~60 | 289,344 | **361,269** | 356,563 | 311,212 | D=2 (+24.9%) |
| 8 | ~60 | 413,592 | **431,316** | 421,474 | 448,184 | D=4 (+8.4%) |
| 9 | ~60 | 429,774 | **473,674** | 475,063 | 452,508 | D=3 (+10.5%) |
| 10 | ~80 | 456,678 | **513,193** | 507,283 | **536,154** | D=4 (+17.4%) |
| 11 | ~80 | 469,576 | **553,417** | 526,410 | 548,381 | D=2 (+17.9%) |
| 12 | ~80 | 405,984 | **414,755** | 425,480 | 406,598 | D=3 (+4.8%) |
| 13 | ~100 | 197,201 | 217,121 | 214,950 | **236,208** | D=4 (+19.8%) |
| 14 | ~100 | 277,356 | **334,893** | 324,536 | **334,893** | D=2/4 (+20.8%) |
| 15 | ~100 | 244,038 | **278,289** | 273,491 | 271,153 | D=2 (+14.0%) |
| **平均** | | **366,073** | **403,314** | **397,263** | **399,709** | |
| **対D=1** | | 基準 | **+10.2%** | **+8.5%** | **+9.2%** | |

- 全15局面で D=2 以上が D=1 より改善（1局面の例外なし）
- D=2 が最も安定して改善。D=3/D=4 は局面依存でブレが大きい
- **最悪局面 (Pos 4, D=2)**: -4.3%（D=1 より悪化）→ ただし D=4 では +18.8%

## HalfKA (v56, L1=512)

EvalFile: `eval/halfka_512x2-8-96_crelu/v56.bin`

| Pos | ply帯 | D=1 | D=2 | D=3 | D=4 | best |
|-----|-------|-----|-----|-----|-----|------|
| 1 | ~20 | 539,781 | **581,951** | 573,766 | 546,670 | D=2 (+7.8%) |
| 2 | ~20 | 586,575 | 585,280 | **606,944** | 588,528 | D=3 (+3.5%) |
| 3 | ~20 | 583,736 | **592,781** | 567,735 | **595,315** | D=4 (+2.0%) |
| 4 | ~40 | 454,506 | **495,727** | **507,918** | 460,636 | D=3 (+11.8%) |
| 5 | ~40 | 493,233 | 496,263 | **503,657** | **516,161** | D=4 (+4.6%) |
| 6 | ~40 | 490,896 | **521,612** | 508,989 | 508,267 | D=2 (+6.3%) |
| 7 | ~60 | 434,028 | 360,994 | **455,861** | 385,343 | D=3 (+5.0%) |
| 8 | ~60 | 576,657 | **632,500** | 611,797 | 610,517 | D=2 (+9.7%) |
| 9 | ~60 | 558,066 | 574,198 | 572,021 | **591,040** | D=4 (+5.9%) |
| 10 | ~80 | 554,104 | **612,679** | **610,336** | 606,957 | D=2 (+10.6%) |
| 11 | ~80 | 564,113 | 595,838 | **599,165** | **611,555** | D=4 (+8.4%) |
| 12 | ~80 | 485,741 | 497,176 | **510,232** | 491,972 | D=3 (+5.0%) |
| 13 | ~100 | 492,447 | **554,386** | 529,852 | 532,667 | D=2 (+12.6%) |
| 14 | ~100 | 427,500 | 440,943 | **446,560** | 439,561 | D=3 (+4.5%) |
| 15 | ~100 | 327,673 | 334,500 | **364,909** | 276,827 | D=3 (+11.4%) |
| **平均** | | **504,604** | **525,122** | **531,316** | **517,468** | |
| **対D=1** | | 基準 | **+4.1%** | **+5.3%** | **+2.6%** | |

- D=3 が平均で最も改善 (+5.3%)
- Pos 7 (D=2): -16.8% と大幅悪化（外れ値）。ただし D=3 では +5.0%
- Pos 15 (D=4): -15.5% と大幅悪化。深すぎる祖先探索がキャッシュミスを誘発

## HalfKP (suisho5, L1=256)

EvalFile: `eval/halfkp_256x2-32-32_crelu/suisho5.bin`

| Pos | ply帯 | D=1 | D=2 | D=3 | D=4 | best |
|-----|-------|-----|-----|-----|-----|------|
| 1 | ~20 | 758,281 | **789,662** | 775,655 | 730,568 | D=2 (+4.1%) |
| 2 | ~20 | 765,461 | **799,128** | **809,092** | **827,970** | D=4 (+8.2%) |
| 3 | ~20 | 765,700 | **786,919** | 779,102 | **825,781** | D=4 (+7.8%) |
| 4 | ~40 | 722,479 | 727,980 | **749,587** | 732,362 | D=3 (+3.8%) |
| 5 | ~40 | 681,674 | **719,759** | **730,112** | **749,979** | D=4 (+10.0%) |
| 6 | ~40 | 744,429 | 712,343 | 718,093 | 692,566 | D=1 (基準) |
| 7 | ~60 | 709,931 | **735,697** | 691,760 | **743,569** | D=4 (+4.7%) |
| 8 | ~60 | 783,198 | **820,212** | **815,294** | **842,030** | D=4 (+7.5%) |
| 9 | ~60 | 813,342 | **835,231** | **852,934** | **885,842** | D=4 (+8.9%) |
| 10 | ~80 | 834,857 | **841,381** | **854,110** | **888,705** | D=4 (+6.4%) |
| 11 | ~80 | 765,930 | 773,307 | **787,270** | 786,154 | D=3 (+2.8%) |
| 12 | ~80 | 658,432 | **702,103** | 664,174 | 670,181 | D=2 (+6.6%) |
| 13 | ~100 | 681,052 | **743,073** | 710,417 | 705,736 | D=2 (+9.1%) |
| 14 | ~100 | 347,880 | 336,894 | 347,880 | 351,703 | D=4 (+1.1%) |
| 15 | ~100 | 410,653 | 355,900 | 377,946 | 384,756 | D=1 (基準) |
| **平均** | | **696,220** | **718,639** | **710,895** | **721,194** | |
| **対D=1** | | 基準 | **+3.2%** | **+2.1%** | **+3.6%** | |

- D=2/D=4 が僅差で D=1 より改善
- Pos 6, 15 では D=1 が最良（MAX_DEPTH を上げると悪化する局面が存在）
- L1=256 では refresh コストが小さいため、効果は限定的

## まとめ

| アーキテクチャ | L1 | 最適 MAX_DEPTH | 平均改善 | 全局面で改善? |
|--------------|-----|---------------|---------|-------------|
| LayerStacks | 1536 | 4 | +43〜60% (別途計測) | - |
| HalfKA_hm | 1024 | **2** | **+10.2%** | ほぼ全局面 |
| HalfKA | 512 | **3** | **+5.3%** | 大半（一部悪化） |
| HalfKP | 256 | **2 or 4** | **+3.2〜3.6%** | 大半（一部悪化） |

### 注意事項

- 個別局面ではMAX_DEPTHを上げると悪化するケースがある（キャッシュミスコスト > refresh 節約）
- 特に終盤の複雑な局面（Pos 14-15）では全アーキテクチャで NPS が低く、MAX_DEPTH の効果も不安定
- 学習プロセス同時実行中のため、個別局面の値は ±5% 程度のブレを含む

## 探索木一致検証コマンド

変更前後で探索木が変わっていないことを確認するためのコマンド。
before バイナリ (`/tmp/rshogi-usi-before`) は変更前のコミットでビルドしたもの。

### before バイナリの準備

```bash
# 例: commit 683c3e5e (AccumulatorCaches 導入前) でビルド
git stash
git checkout 683c3e5e -- crates/
cargo build --release -p rshogi-usi
cp target/release/rshogi-usi /tmp/rshogi-usi-before
git checkout HEAD -- crates/
git stash pop
```

### 検証スクリプト

```bash
EVAL="/mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin"
PROGRESS="/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin"

head -10 /tmp/bench_positions.txt | while IFS= read -r pos_line; do
    total=$((${total:-0} + 1))

    # before
    tmpout_b=$(mktemp); tmpfifo_b=$(mktemp -u); mkfifo "$tmpfifo_b"
    (
        printf "usi\nsetoption name Threads value 1\nsetoption name Hash value 256\nsetoption name EvalFile value %s\nsetoption name LS_BUCKET_MODE value progress8kpabs\nsetoption name LS_PROGRESS_COEFF value %s\nisready\n%s\ngo depth 20\n" "$EVAL" "$PROGRESS" "$pos_line"
        while ! grep -q "^bestmove" "$tmpout_b" 2>/dev/null; do sleep 0.1; done
        printf "quit\n"
    ) > "$tmpfifo_b" &
    timeout 120 /tmp/rshogi-usi-before < "$tmpfifo_b" > "$tmpout_b" 2>&1
    wait $! 2>/dev/null || true

    # after
    tmpout_a=$(mktemp); tmpfifo_a=$(mktemp -u); mkfifo "$tmpfifo_a"
    (
        printf "usi\nsetoption name Threads value 1\nsetoption name Hash value 256\nsetoption name EvalFile value %s\nsetoption name LS_BUCKET_MODE value progress8kpabs\nsetoption name LS_PROGRESS_COEFF value %s\nisready\n%s\ngo depth 20\n" "$EVAL" "$PROGRESS" "$pos_line"
        while ! grep -q "^bestmove" "$tmpout_a" 2>/dev/null; do sleep 0.1; done
        printf "quit\n"
    ) > "$tmpfifo_a" &
    timeout 120 target/release/rshogi-usi < "$tmpfifo_a" > "$tmpout_a" 2>&1
    wait $! 2>/dev/null || true

    b_bm=$(grep "^bestmove" "$tmpout_b" | head -1)
    a_bm=$(grep "^bestmove" "$tmpout_a" | head -1)
    b_info=$(grep "info depth 20 " "$tmpout_b" | head -1 | grep -oP 'score cp [+-]?\d+|nodes \d+')
    a_info=$(grep "info depth 20 " "$tmpout_a" | head -1 | grep -oP 'score cp [+-]?\d+|nodes \d+')

    if [ "$b_bm" = "$a_bm" ] && [ "$b_info" = "$a_info" ]; then
        echo "Position $total: MATCH ($b_bm, $b_info)"
    else
        echo "Position $total: MISMATCH"
        echo "  before: $b_bm | $b_info"
        echo "  after:  $a_bm | $a_info"
    fi

    rm -f "$tmpout_b" "$tmpfifo_b" "$tmpout_a" "$tmpfifo_a"
done
```

### 検証ポイント

- bestmove、score cp、nodes が全局面で完全一致すること
- depth 20 で検証（浅い depth では偶然一致する可能性があるため）
- 局面は `/tmp/bench_positions.txt`（実対局棋譜から ply 20/40/60/80/100 帯を各3局面抽出）

## rshogi vs YaneuraOu NPS 比較ベンチマーク

### 局面準備

```bash
# start_sfens_ply32.txt の先頭 20 局面を使用
head -20 start_sfens_ply32.txt > /tmp/bench_20pos.txt
```

### 計測コマンド

```bash
# rshogi
cargo run -p tools --release --bin benchmark -- \
  --engine target/release/rshogi-usi \
  --usi-option "EvalFile=/mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin" \
  --usi-option "LS_BUCKET_MODE=progress8kpabs" \
  --usi-option "LS_PROGRESS_COEFF=/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin" \
  --limit-type depth --limit 20 --tt-mb 256 \
  --sfens /tmp/bench_20pos.txt --iterations 3 -v

# YaneuraOu V2
cargo run -p tools --release --bin benchmark -- \
  --engine /mnt/nvme1/development/YaneuraOu/source/YaneuraOu-sfnnwop1536-v2 \
  --usi-option "EvalDir=/mnt/nvme1/development/YaneuraOu/source/eval" \
  --usi-option "FV_SCALE=28" \
  --usi-option "LS_BUCKET_MODE=progress8kpabs" \
  --usi-option "LS_PROGRESS_COEFF=/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin" \
  --usi-option "BookFile=no_book" \
  --limit-type depth --limit 20 --tt-mb 256 \
  --sfens /tmp/bench_20pos.txt --iterations 3 -v
```

### 条件

- 同一モデル: v82-300 (LayerStack 1536x16x32, progress8kpabs, FV_SCALE=28)
- depth 20, Threads=1, Hash=256MB
- 20 局面 × 3 iterations
- `start_sfens_ply32.txt` の先頭 20 局面（sfen プレフィックス自動除去対応済み）
- YO 側: `EvalDir` + `nn.bin` symlink、`BookFile=no_book`、`FV_SCALE=28` 明示指定
- 学習プロセスなしのクリーン環境で実施すること

### 計測結果 (2026-03-27, クリーン環境)

| エンジン | Total Nodes | Total Time | Avg NPS | 対 rshogi 比 |
|---------|------------|-----------|---------|-------------|
| rshogi | 327,857,578 | 553,003ms | 592,867 | 基準 |
| YO V2 | 124,008,003 | 195,580ms | 634,052 | **+6.9%** |

- YO が rshogi より 6.9% 高速（perf stat 調査の 6.8% とほぼ一致）
- ノード数の差は探索木の違い（同一 depth でも枝刈り判断が異なる）
- 以前の計測で「rshogi が 34% 速い」は学習プロセス同時実行 + depth 20 到達の非対称性による誤計測だった

## 2026-03-27 LayerStack propagate explicit scratch 候補

`LayerStackBucket::propagate()` を YO に寄せて、`fc_0 -> ClippedReLU / SqrClippedReLU -> fc_1 -> ClippedReLU -> fc_2` の中間を scratch buffer へ明示的に展開する候補を実装した。
`l2_input` / `l2_relu` は `AffineTransform::propagate()` の入力アライメント制約を満たす必要があるため、`Aligned<[u8; N]>` にしている。最初の版は未整列 buffer で `cargo test` 中に SIGSEGV したため修正した。

### 実施コマンド

```bash
cargo fmt && cargo clippy --fix --allow-dirty --tests && cargo test
cargo build --release --bin bench_nnue_eval
target/release/bench_nnue_eval --nnue-file /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin --mode layer-stack-propagate --ls-bucket-mode progress8kpabs --ls-progress-coeff /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin --warmup 10000 --iterations 500000
target/release/bench_nnue_eval --nnue-file /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin --mode layer-stack-eval --ls-bucket-mode progress8kpabs --ls-progress-coeff /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin --warmup 10000 --iterations 500000
```

### 結果

ベースラインは同日同条件で取得済みの値（`layer-stack-propagate = 261.8 ns/op`, `layer-stack-eval = 327.6 ns/op`）。

| mode | before | after | diff |
|------|--------|-------|------|
| `layer-stack-propagate` | 261.8 ns/op | 251.7 ns/op | `-3.9%` |
| `layer-stack-eval` | 327.6 ns/op | 306.0 ns/op | `-6.6%` |

### ここまでの事実

- microbench では明確に改善したので search-only A/B に進める価値がある
- generic helper 合成だけでは負けたが、buffer 配置を含めて組み直すと挙動が変わる
- `AffineTransform::propagate()` に未整列の scratch buffer を渡すと AVX2 環境で落ちる

### search-only A/B

比較条件:

- baseline: `/tmp/rshogi-usi-layerstack-baseline`
- candidate: `target/release/rshogi-usi`
- `perf stat -D -1 --control fifo:...` による search-only 計測
- `taskset -c 4`, `Threads=1`, `USI_Hash=256`, `LS_BUCKET_MODE=progress8kpabs`
- 順序依存ノイズを見るため `baseline -> candidate`, `candidate -> baseline` を実施

結果:

| order | engine | final nps | cycles / node | instructions / node |
| --- | --- | ---: | ---: | ---: |
| 1 | baseline | `576,300` | `7,266.0` | `15,808.1` |
| 1 | candidate | `593,834` | `7,115.8` | `14,625.9` |
| 2 | candidate | `590,345` | `7,138.9` | `14,628.4` |
| 2 | baseline | `579,743` | `7,238.9` | `15,798.1` |

平均:

| engine | avg nps | avg cycles / node | avg instructions / node |
| --- | ---: | ---: | ---: |
| baseline | `578,021.5` | `7,252.5` | `15,803.1` |
| candidate | `592,089.5` | `7,127.4` | `14,627.2` |

search-only の事実:

- candidate は 2-order 平均で baseline 比 `+2.43%`
- `cycles / node` は約 `-1.73%`
- `instructions / node` は約 `-7.44%`

### 探索木一致検証

`/tmp/bench_positions.txt` の先頭 10 局面、depth 20 で before/after を比較。

結果:

- 10/10 局面で `score cp` または `nodes` が不一致
- `bestmove` は一致する局面も多いが、ponder や探索量が崩れており採用不可

代表例:

| position | before | after |
| --- | --- | --- |
| 1 | `bestmove P*7f ponder 7g8h`, `score cp 256`, `nodes 2074779` | `bestmove P*7f ponder 7g8h`, `score cp 111`, `nodes 1406680` |
| 4 | `bestmove B*3h ponder 2i7i`, `score cp 698`, `nodes 3638766` | `bestmove B*3h ponder G*3b`, `score cp 324`, `nodes 5855382` |
| 8 | `bestmove 6g7h ponder S*6b`, `score cp 580`, `nodes 803223` | `bestmove 6g7h ponder 7i7h`, `score cp 794`, `nodes 4563145` |

判断:

- 第1版は「速いが探索木を変える」ため不採用
- 原因候補は `sqr_clipped_relu_explicit()` の数値経路変更で、次はそこだけ厳密式へ戻して再計測する

## 2026-03-27 LayerStack propagate explicit scratch 候補 第2版（`sqr` 厳密式に戻す）

第1版の探索木不一致を受け、`sqr_clipped_relu_explicit()` の SIMD 経路を外し、
`((input as i64 * input as i64) >> 19).clamp(0, 127)` の厳密式だけに戻した。
scratch/buffer 再編と `ClippedReLU` の SIMD 化は残している。

### 実施コマンド

```bash
cargo fmt && cargo clippy --fix --allow-dirty --tests && cargo test
cargo build --release --bin bench_nnue_eval
target/release/bench_nnue_eval --nnue-file /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin --mode layer-stack-propagate --ls-bucket-mode progress8kpabs --ls-progress-coeff /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin --warmup 10000 --iterations 500000
target/release/bench_nnue_eval --nnue-file /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin --mode layer-stack-eval --ls-bucket-mode progress8kpabs --ls-progress-coeff /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin --warmup 10000 --iterations 500000
```

### 結果

| mode | before | after | diff |
|------|--------|-------|------|
| `layer-stack-propagate` | 261.8 ns/op | 267.9 ns/op | `+2.3%` |
| `layer-stack-eval` | 327.6 ns/op | 300.9 ns/op | `-8.1%` |

### ここまでの事実

- `propagate` 単体では悪化したが、`layer-stack-eval` 全体ではまだ改善している
- 第1版の勝因は `sqr` SIMD helper 単体だけではなく、scratch/buffer 再編や `ClippedReLU` 側にもある
- 探索木一致を満たす可能性は第2版の方が高いので、search-only A/B を続けて確認する

### search-only A/B

| order | engine | final nps | cycles / node | instructions / node |
| --- | --- | ---: | ---: | ---: |
| 1 | baseline | `615,208` | `7,140.8` | `15,812.0` |
| 1 | candidate | `621,298` | `7,087.0` | `14,638.0` |
| 2 | candidate | `613,539` | `7,166.2` | `14,631.5` |
| 2 | baseline | `599,134` | `7,319.9` | `15,818.0` |

平均:

| engine | avg nps | avg cycles / node | avg instructions / node |
| --- | ---: | ---: | ---: |
| baseline | `607,171.0` | `7,230.4` | `15,815.0` |
| candidate | `617,418.5` | `7,126.6` | `14,634.8` |

search-only の事実:

- candidate は 2-order 平均で baseline 比 `+1.69%`
- `instructions / node` は約 `-7.46%`
- `cycles / node` は約 `-1.43%`

### 探索木一致検証

結果:

- 第1版と同じく 10/10 局面で不一致
- 代表値も第1版と同じ系列になっており、`sqr` を厳密式へ戻しても探索木不一致は解消しなかった

判断:

- 第2版も不採用
- 問題は `sqr` だけでなく scratch/buffer 再編そのもの、またはその codegen にある

## 2026-03-27 LayerStack propagate explicit scratch 候補 第3版（`propagate()` の scalar reference テスト追加）

第2版で探索木不一致が残ったため、`LayerStackBucket::propagate()` 自体を
scalar reference と比較する unit test を追加し、差分経路を局所化した。
結果として、`sqr` を `l2_input` 先頭へ直接書き込む形だと `propagate()` の最終値が
reference と一致せず、`l2_sqr` 一時配列へ作ってから `copy_from_slice` する形で一致した。

追加したテスト:

- `test_layer_stack_l2_input_matches_scalar_reference`
- `test_layer_stack_l2_relu_matches_scalar_reference`
- `test_layer_stack_bucket_propagate_matches_scalar_reference`

### 実施コマンド

```bash
cargo fmt && cargo clippy --fix --allow-dirty --tests && cargo test
cargo build --release --bin bench_nnue_eval
target/release/bench_nnue_eval --nnue-file /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin --mode layer-stack-propagate --ls-bucket-mode progress8kpabs --ls-progress-coeff /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin --warmup 10000 --iterations 500000
target/release/bench_nnue_eval --nnue-file /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin --mode layer-stack-eval --ls-bucket-mode progress8kpabs --ls-progress-coeff /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin --warmup 10000 --iterations 500000
RUSTFLAGS='-C target-cpu=native' CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
```

### microbench 結果

| mode | before | after | diff |
|------|--------|-------|------|
| `layer-stack-propagate` | 261.8 ns/op | 267.9 ns/op | `+2.3%` |
| `layer-stack-eval` | 327.6 ns/op | 316.8 ns/op | `-3.3%` |

### search-only A/B

| order | engine | final nps | cycles / node | instructions / node |
| --- | --- | ---: | ---: | ---: |
| 1 | baseline | `598,320` | `7,431.5` | `15,834.9` |
| 1 | candidate | `549,091` | `8,031.4` | `15,796.0` |
| 2 | candidate | `614,385` | `7,181.9` | `15,815.0` |
| 2 | baseline | `606,660` | `7,257.5` | `15,817.7` |

平均:

| engine | avg nps | avg cycles / node | avg instructions / node |
| --- | ---: | ---: | ---: |
| baseline | `602,490.0` | `7,344.5` | `15,826.3` |
| candidate | `581,738.0` | `7,606.7` | `15,805.5` |

### ここまでの事実

- unit test 上は `propagate()` と scalar reference の一致を回復した
- しかし search-only 平均では baseline 比 `-3.44%`
- `instructions / node` はほぼフラットで、悪化の主因は `cycles / node` 増

判断:

- 第3版も不採用
- scratch/buffer 再編系の案は、少なくとも現状の形では end-to-end で勝てない

## 2026-03-27 MovePicker score loop / partial sort 軽量化候補

`MovePicker::next_move()` と `partial_insertion_sort()` が依然として search-only `perf record` 上位だったため、探索順序を変えずにホットパスの実装だけを軽くする候補を試した。

変更点:

- `score_captures()` / `score_quiets()` / `score_evasions()` を `ExtMoveBuffer::get/set_value` 反復から slice 直アクセスへ変更
- `score_quiets()` の continuation history 参照と `LowPlyHistory` 分岐をループ外へ移動
- `partial_insertion_sort()` を pointer ベースの同型実装へ変更
- `piece_value()` を `#[inline]`

baseline バイナリは変更前の current tree からコピーした `/tmp/rshogi-usi-before-movepicker-opt` を使用した。

### 事前確認用 search-only perf record

```bash
taskset -c 4 target/release/rshogi-usi
perf record -g -F 997 -p "$ENGINE_PID" -o /tmp/perf-current.data -- sleep 10
perf report -i /tmp/perf-current.data --stdio --no-children -g none --percent-limit 0.5 | head -120
```

上位関数:

- `LayerStackBucket::propagate 11.39%`
- `update_accumulator_with_cache 8.11%`
- `refresh_perspective_with_cache 8.06%`
- `MovePicker::next_move 5.70%`
- `partial_insertion_sort 4.18%`

### 実施コマンド

```bash
cargo fmt && cargo clippy --fix --allow-dirty --tests && cargo test
cp target/release/rshogi-usi /tmp/rshogi-usi-before-movepicker-opt
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
```

search-only A/B:

```bash
POS_LINE="$(head -1 /tmp/bench_positions.txt)"
EVAL=/mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin
PROGRESS=/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin

# run_case <engine>
# - Threads=1 / Hash=256 / LS_BUCKET_MODE=progress8kpabs
# - fixed position + `go movetime 10000`
# - `perf stat -p $ENGINE_PID -- sleep 10` で search-only 近傍を採取
```

### search-only A/B

| order | engine | final nps | cycles / node | instructions / node |
| --- | --- | ---: | ---: | ---: |
| 1 | baseline | `546,836` | `8,015.7` | `17,303.4` |
| 1 | candidate | `548,977` | `7,964.5` | `17,080.9` |
| 2 | candidate | `548,149` | `7,945.3` | `17,054.6` |
| 2 | baseline | `525,950` | `8,305.6` | `17,318.9` |

平均:

| engine | avg nps | avg cycles / node | avg instructions / node |
| --- | ---: | ---: | ---: |
| baseline | `536,393.0` | `8,160.7` | `17,311.2` |
| candidate | `548,563.0` | `7,954.9` | `17,067.8` |

search-only の事実:

- candidate は 2-order 平均で baseline 比 `+2.27%`
- `instructions / node` は約 `-1.41%`
- `cycles / node` は約 `-2.52%`
- 少なくともこの局面・この負荷条件では、MovePicker ホットパス軽量化はプラス

### 探索木一致検証

検証コマンドは上の「探索木一致検証コマンド」と同型で、`before` に `/tmp/rshogi-usi-before-movepicker-opt`、`after` に `target/release/rshogi-usi` を指定した。

結果:

- 10/10 局面で `bestmove` / `score cp` / `nodes` が完全一致
- 代表例:
  - `MATCH 1 bestmove P*7f ponder 7g8h | score cp 256|nodes 2074779|`
  - `MATCH 4 bestmove B*3h ponder 2i7i | score cp 698|nodes 3638766|`
  - `MATCH 10 bestmove S*9d ponder 9c9b | score cp -3071|nodes 4839031|`

判断:

- 採用
- 探索順序を変えずに `MovePicker` 周辺の instruction/cycle を削れた

## 2026-03-27 progress8kpabs index 計算軽量化候補

`compute_progress8kpabs_sum()` と `update_progress8kpabs_sum_diff()` は、同じ king-square 行の重み参照に対して都度 `sq * FE_OLD_END + idx` を計算していた。加えて `progress_sum_to_bucket()` は iterator/filter/count ベースだったため、固定長比較へ寄せた。

変更点:

- `weights_b` / `weights_w` の行 slice を先に切って乗算を除去
- diff update 側も同じ row slice を使用
- `progress_sum_to_bucket()` を `partition_point()` ベースへ変更

baseline バイナリは変更前の current tree からコピーした `/tmp/rshogi-usi-before-progress8kpabs-opt` を使用した。

### 実施コマンド

```bash
cargo fmt && cargo clippy --fix --allow-dirty --tests && cargo test
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
```

search-only A/B:

```bash
POS_LINE="$(head -1 /tmp/bench_positions.txt)"
EVAL=/mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin
PROGRESS=/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin

# run_case <engine>
# - Threads=1 / Hash=256 / LS_BUCKET_MODE=progress8kpabs
# - fixed position + `go movetime 10000`
# - `perf stat -p $ENGINE_PID -- sleep 10`
```

### search-only A/B

| order | engine | final nps | cycles / node | instructions / node |
| --- | --- | ---: | ---: | ---: |
| 1 | baseline | `542,410` | `8,117.4` | `17,058.4` |
| 1 | candidate | `545,682` | `8,049.5` | `17,027.2` |
| 2 | candidate | `548,668` | `7,986.8` | `17,047.8` |
| 2 | baseline | `533,187` | `8,211.9` | `17,063.8` |

平均:

| engine | avg nps | avg cycles / node | avg instructions / node |
| --- | ---: | ---: | ---: |
| baseline | `537,798.5` | `8,164.7` | `17,061.1` |
| candidate | `547,175.0` | `8,018.2` | `17,037.5` |

search-only の事実:

- candidate は 2-order 平均で baseline 比 `+1.74%`
- `cycles / node` は約 `-1.79%`
- `instructions / node` は約 `-0.14%` と小さく、主因は address 計算と bucket 化の軽量化による cycle 減

### 探索木一致検証

検証コマンドは上の「探索木一致検証コマンド」と同型で、`before` に `/tmp/rshogi-usi-before-progress8kpabs-opt`、`after` に `target/release/rshogi-usi` を指定した。

結果:

- 10/10 局面で `bestmove` / `score cp` / `nodes` が完全一致
- 代表例:
  - `MATCH 1 bestmove P*7f ponder 7g8h | score cp 256|nodes 2074779|`
  - `MATCH 6 bestmove P*4e ponder B*4g | score cp -510|nodes 4357156|`
  - `MATCH 10 bestmove S*9d ponder 9c9b | score cp -3071|nodes 4839031|`

判断:

- 採用
- `progress8kpabs` の軽量化は小さいが再現性のあるプラス

## 2026-03-27 search_node PV clone 除去候補

`perf` の `__memmove_avx_unaligned_erms` と [alpha_beta.rs](/mnt/nvme1/development/rshogi/crates/rshogi-core/src/search/alpha_beta.rs) の
`st.stack[(ply + 1)].pv.clone()` が対応している可能性を疑い、PV 更新時の clone を
`split_at_mut()` による disjoint borrow へ置き換えた。

変更点:

- `let child_pv = st.stack[(ply + 1) as usize].pv.clone();`
- `st.stack[ply as usize].update_pv(mv, &child_pv);`

を

- `let (head, tail) = st.stack.split_at_mut(child_idx);`
- `head[ply as usize].update_pv(mv, &tail[0].pv);`

へ変更

baseline バイナリは変更前の current tree からコピーした `/tmp/rshogi-usi-before-pvclone-opt` を使用した。

### 実施コマンド

```bash
cargo fmt && cargo clippy --fix --allow-dirty --tests && cargo test
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
```

search-only A/B:

```bash
POS_LINE="$(head -1 /tmp/bench_positions.txt)"
EVAL=/mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin
PROGRESS=/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin

# run_case <engine>
# - Threads=1 / Hash=256 / LS_BUCKET_MODE=progress8kpabs
# - fixed position + `go movetime 10000`
# - `perf stat -p $ENGINE_PID -- sleep 10`
```

### search-only A/B

| order | engine | final nps | cycles / node | instructions / node |
| --- | --- | ---: | ---: | ---: |
| 1 | baseline | `621,379` | `7,549.8` | `16,979.1` |
| 1 | candidate | `598,347` | `7,860.3` | `17,002.6` |
| 2 | candidate | `601,012` | `7,828.5` | `17,010.3` |
| 2 | baseline | `596,522` | `7,876.2` | `17,017.2` |

平均:

| engine | avg nps | avg cycles / node | avg instructions / node |
| --- | ---: | ---: | ---: |
| baseline | `608,950.5` | `7,713.0` | `16,998.2` |
| candidate | `599,679.5` | `7,844.4` | `17,006.5` |

search-only の事実:

- candidate は 2-order 平均で baseline 比 `-1.52%`
- `cycles / node` は悪化
- `instructions / node` も微増で、`clone` 除去の期待には反した

判断:

- 不採用
- 少なくともこの形の `split_at_mut()` 化は codegen が悪く、`pv.clone()` より遅い

## 2026-03-27 search_node PV clone 除去候補 その2

`split_at_mut()` 版の codegen 退化を避けるため、同じ目的を raw pointer で試した。
対象は同じく [alpha_beta.rs](/mnt/nvme1/development/rshogi/crates/rshogi-core/src/search/alpha_beta.rs) の
PV 更新部で、`child_pv` の一時 `clone()` を使わず
`st.stack.as_mut_ptr()` から `stack[ply]` と `stack[ply + 1]` を直接参照した。

変更意図:

- `pv.clone()` が `__memmove_avx_unaligned_erms` の主因かを再確認
- `split_at_mut()` より単純な codegen なら改善するかを確認

baseline バイナリは同じく `/tmp/rshogi-usi-before-pvclone-opt` を使用した。

### 実施コマンド

```bash
cargo fmt && cargo clippy --fix --allow-dirty --tests && cargo test
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
```

search-only A/B:

```bash
POS_LINE="$(head -1 /tmp/bench_positions.txt)"
EVAL=/mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin
PROGRESS=/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin

# run_case <engine>
# - Threads=1 / Hash=256 / LS_BUCKET_MODE=progress8kpabs
# - fixed position + `go movetime 10000`
# - `perf stat -p $ENGINE_PID -- sleep 10`
```

### search-only A/B

| order | engine | final nps | cycles / node | instructions / node |
| --- | --- | ---: | ---: | ---: |
| 1 | baseline | `625,849` | `7,502.4` | `16,989.3` |
| 1 | candidate | `624,408` | `7,540.4` | `17,083.3` |
| 2 | candidate | `622,445` | `7,555.0` | `17,065.9` |
| 2 | baseline | `620,810` | `7,573.4` | `17,004.0` |

平均:

| engine | avg nps | avg cycles / node | avg instructions / node |
| --- | ---: | ---: | ---: |
| baseline | `623,329.5` | `7,537.9` | `16,996.7` |
| candidate | `623,426.5` | `7,547.7` | `17,074.6` |

search-only の事実:

- candidate は 2-order 平均で baseline 比 `+0.02%` と実質ノイズ
- `cycles / node` は微悪化
- `instructions / node` は `+0.46%` 悪化

判断:

- 不採用
- `pv.clone()` は現状の `__memmove` 主因とは言いにくい

## 2026-03-27 refresh_perspective_with_cache の active 収集コピー除去

`pv.clone()` 候補が外れたので、現行 baseline で search-only `perf record -g` を取り直した。
この時点の上位は以下だった。

- `LayerStackBucket::propagate` `11.63%`
- `MovePicker::next_move` `9.37%`
- `update_accumulator_with_cache` `8.67%`
- `refresh_perspective_with_cache` `7.58%`
- `SearchWorker::search_node` `5.85%`
- `Position::attackers_to_occ` `4.98%`
- `__memmove_avx_unaligned_erms` `4.78%`

### 事実確認に使ったコマンド

```bash
POS_LINE="$(head -1 /tmp/bench_positions.txt)"
EVAL=/mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin
PROGRESS=/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin

# search-only call graph
perf record -g -F 999 -o /tmp/rshogi-search-only-callgraph.data -p "$ENGINE_PID" -- sleep 10
perf report --stdio -i /tmp/rshogi-search-only-callgraph.data --no-children --percent-limit 0.5

# annotate
perf annotate --stdio -i /tmp/rshogi-search-only-callgraph.data \
  'rshogi_core::nnue::feature_transformer_layer_stacks::FeatureTransformerLayerStacks::update_accumulator_with_cache'
perf annotate --stdio -i /tmp/rshogi-search-only-callgraph.data \
  '_ZN11rshogi_core4nnue32feature_transformer_layer_stacks29FeatureTransformerLayerStacks30refresh_perspective_with_cache17hadc67e55075560d6E.llvm.11645232554315537171'
```

### perf / annotate で分かった事実

- `update_accumulator_with_cache` 内の `curr.copy_from_slice(prev)` は `memcpy(0xc00)` になっている
- ただしこの経路は以前 explicit copy で負けているため、そのまま再挑戦する筋は弱い
- `refresh_perspective_with_cache` では `get_active_features()` の戻り値 `IndexList` が `memcpy(0x1b8)` でローカルへコピーされている
- 同じ関数で `sorted_buf = [0u32; MAX_ACTIVE_FEATURES]` の全 zero fill も入っている

判断:

- `refresh_perspective_with_cache` の active 収集まわりは、search-only hot path に対して筋が良い
- 対策は 2 点に限定する
  - `IndexList` 返却コピーをやめ、呼び出し側のローカルへ直接 `append_active_indices()`
  - `sorted_buf` を `MaybeUninit<[u32; MAX_ACTIVE_FEATURES]>` 相当で使用領域だけ初期化

### 実装内容

[feature_transformer_layer_stacks.rs](/mnt/nvme1/development/rshogi/crates/rshogi-core/src/nnue/feature_transformer_layer_stacks.rs)

- `append_active_indices()` ヘルパーを追加
- `refresh_accumulator()` / `update_accumulator()` / `refresh_perspective_with_cache()` で `get_active_features()` 返却を廃止
- `refresh_perspective_with_cache()` の `sorted_buf` を `MaybeUninit<u32>` 配列へ変更し、`len` 要素のみ初期化

### 実施コマンド

```bash
cargo fmt && cargo clippy --fix --allow-dirty --tests && cargo test
cargo build --release --bin bench_nnue_eval
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
cp target/release/rshogi-usi /tmp/rshogi-usi-before-refresh-active-opt
cp target/release/bench_nnue_eval /tmp/bench_nnue_eval-before-refresh-active-opt
```

### microbench

```bash
NNUE=/mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin
PROGRESS=/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin

for engine in /tmp/bench_nnue_eval-before-refresh-active-opt target/release/bench_nnue_eval; do
  "$engine" --nnue-file "$NNUE" --ls-bucket-mode progress8kpabs --ls-progress-coeff "$PROGRESS" \
    --mode layer-stack-refresh-cache --warmup 20000 --iterations 300000
  "$engine" --nnue-file "$NNUE" --ls-bucket-mode progress8kpabs --ls-progress-coeff "$PROGRESS" \
    --mode layer-stack-update-cache --warmup 20000 --iterations 300000
done
```

結果:

| bench | baseline ns/op | candidate ns/op | delta |
| --- | ---: | ---: | ---: |
| `layer-stack-refresh-cache` | `2776.3` | `2440.4` | `+12.10%` |
| `layer-stack-update-cache` | `167.8` | `178.6` | `-6.44%` |

microbench の事実:

- refresh path 単体では大幅改善
- update path 単体では悪化
- この時点では mixed なので、採否は search-only A/B で決める

### search-only A/B

baseline は `/tmp/rshogi-usi-before-refresh-active-opt`、candidate は `target/release/rshogi-usi`。

```bash
POS_LINE="$(head -1 /tmp/bench_positions.txt)"
EVAL=/mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin
PROGRESS=/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin

# run_case <engine>
# - Threads=1 / Hash=256 / LS_BUCKET_MODE=progress8kpabs
# - fixed position + `go movetime 10000`
# - `perf stat -p $ENGINE_PID -- sleep 10`
```

| order | engine | final nps | cycles / node | instructions / node |
| --- | --- | ---: | ---: | ---: |
| 1 | baseline | `592,090` | `7,942.9` | `17,046.7` |
| 1 | candidate | `601,085` | `7,814.4` | `16,987.3` |
| 2 | candidate | `604,878` | `7,762.2` | `16,987.9` |
| 2 | baseline | `587,123` | `7,999.5` | `17,065.9` |

平均:

| engine | avg nps | avg cycles / node | avg instructions / node |
| --- | ---: | ---: | ---: |
| baseline | `589,606.5` | `7,971.2` | `17,056.3` |
| candidate | `602,981.5` | `7,788.3` | `16,987.6` |

search-only の事実:

- candidate は 2-order 平均で baseline 比 `+2.27%`
- `cycles / node` は `-2.29%`
- `instructions / node` は `-0.40%`

### 探索木一致検証

検証コマンドは上の「探索木一致検証コマンド」と同型で、`before` に
`/tmp/rshogi-usi-before-refresh-active-opt`、`after` に `target/release/rshogi-usi` を指定した。

結果:

- 10/10 局面で `bestmove` / `score cp` / `nodes` が完全一致
- 例:
  - `MATCH 1 bestmove P*7f ponder 7g8h | score cp 256|nodes 2074779|`
  - `MATCH 6 bestmove P*4e ponder B*4g | score cp -510|nodes 4357156|`
  - `MATCH 10 bestmove S*9d ponder 9c9b | score cp -3071|nodes 4839031|`

判断:

- 採用
- `refresh_perspective_with_cache` の active 収集コピー除去は、局所ベンチ混在でも search-only では再現性のあるプラス
