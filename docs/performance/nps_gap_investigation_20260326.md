# rshogi vs YaneuraOu NPS 差異調査ログ

日付: 2026-03-26

## 目的

同一 NNUE モデル・同一 bucket mode・Threads=1 の条件で、rshogi が YaneuraOu より低い NPS を示す原因を実測ベースで切り分ける。

## 測定方針

1. 再現条件を固定して NPS を再計測する
2. `perf stat` で IPC / instruction 数 / cache miss 傾向を比較する
3. `perf record` でホットスポットを比較する
4. 必要に応じて NNUE ホットループの生成コードを比較する

## 測定環境

- ホスト: AMD Ryzen 9 5950X 16-Core Processor
- OS: Linux 6.8.0-90-generic
- `perf`: 6.8.12
- CPU governor: `schedutil`
- 論理 CPU 数: 32
- 計測時 load average: `7.68, 7.55, 7.89`
- 常駐高負荷プロセス: `target/release/examples/shogi_layerstack ... --threads 20` が約 697% CPU
- カレントディレクトリ: `/mnt/nvme1/development/rshogi`
- rshogi: `target/release/rshogi-usi`
- YaneuraOu: `/mnt/nvme1/development/YaneuraOu/source/YaneuraOu-sfnnwop1536-v2`
- Eval: `/mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin`
- Progress coeff: `/mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin`
- YaneuraOu 側は `EvalFile` オプション非対応。`source/eval/nn.bin` が上記 eval への symlink になっていることを確認済み
- YaneuraOu 側は book 読み込みノイズを避けるため `BookFile=no_book` を指定

## 実行ログ

### 0. `perf` 権限疎通確認

コマンド:

```bash
perf stat -e cycles,instructions sleep 0.1
perf record -o /tmp/perf-test.data -- sleep 0.1
perf report --stdio -i /tmp/perf-test.data --header-only
```

結果:

- `perf stat`: 成功
- `perf record`: 成功
- `perf report`: 成功

解釈:

- 今回のセッションでは `sudo` なしで `perf stat` / `perf record` を実行可能

### 1. ベースライン NPS 再計測

まず 1 回だけ depth 20 を確認したところ、YaneuraOu 側の `EvalFile` 指定は無効だった。
以後、以下の条件に固定した。

- 両エンジンとも `Threads=1`, `USI_Hash=256`
- `taskset -c 4` で同一コアに固定
- rshogi は `EvalFile` を指定
- YaneuraOu は `BookFile=no_book` を指定し、`eval/nn.bin` symlink を使用

実行コマンド（rshogi, 3回）:

```bash
tmpout=$(mktemp); tmpfifo=$(mktemp -u); mkfifo "$tmpfifo"
(
  printf "usi\nsetoption name Threads value 1\nsetoption name USI_Hash value 256\nsetoption name EvalFile value /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin\nsetoption name LS_BUCKET_MODE value progress8kpabs\nsetoption name LS_PROGRESS_COEFF value /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin\nisready\nposition startpos\ngo depth 20\n"
  while ! grep -q "^bestmove" "$tmpout" 2>/dev/null; do sleep 0.1; done
  printf "quit\n"
) > "$tmpfifo" &
timeout 120 taskset -c 4 /mnt/nvme1/development/rshogi/target/release/rshogi-usi < "$tmpfifo" > "$tmpout" 2>&1
wait $! 2>/dev/null || true
grep "^info depth " "$tmpout" | tail -1
rm -f "$tmpout" "$tmpfifo"
```

実行コマンド（YaneuraOu, 3回）:

```bash
tmpout=$(mktemp); tmpfifo=$(mktemp -u); mkfifo "$tmpfifo"
(
  printf "usi\nsetoption name Threads value 1\nsetoption name USI_Hash value 256\nsetoption name BookFile value no_book\nsetoption name LS_BUCKET_MODE value progress8kpabs\nsetoption name LS_PROGRESS_COEFF value /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin\nisready\nposition startpos\ngo depth 20\n"
  while ! grep -q "^bestmove" "$tmpout" 2>/dev/null; do sleep 0.1; done
  printf "quit\n"
) > "$tmpfifo" &
timeout 120 taskset -c 4 /mnt/nvme1/development/YaneuraOu/source/YaneuraOu-sfnnwop1536-v2 < "$tmpfifo" > "$tmpout" 2>&1
wait $! 2>/dev/null || true
grep "^info depth " "$tmpout" | tail -1
rm -f "$tmpout" "$tmpfifo"
```

結果:

| engine | run | final info line |
| --- | --- | --- |
| rshogi | 1 | `info depth 20 ... nodes 383432 time 660 nps 580957 ...` |
| rshogi | 2 | `info depth 20 ... nodes 383432 time 636 nps 602880 ...` |
| rshogi | 3 | `info depth 20 ... nodes 383432 time 616 nps 622454 ...` |
| YO | 1 | `info depth 19 ... nodes 310831 time 454 nps 684649 ...` |
| YO | 2 | `info depth 19 ... nodes 310831 time 439 nps 708043 ...` |
| YO | 3 | `info depth 19 ... nodes 310831 time 436 nps 712915 ...` |

集計:

- rshogi 平均 NPS: `602,097`
- YaneuraOu 平均 NPS: `701,869`
- 比率: `1.166x`（この負荷環境では YO が約 16.6% 高速）

注意:

- この 3 回計測では rshogi は `depth 20`、YaneuraOu は最終 `info` が `depth 19` で止まった
- bestmove までは正常に返るため、ログ仕様差または探索差の可能性がある
- このため、以降の `perf` 比較は startup コストの影響も避けるため `go movetime 10000` に切り替えた

### 2. `perf stat` 比較

最初に depth 20 の wrapper 全体で `perf stat` を取ったが、初期化コストの寄与がまだ大きいと判断した。
そのため、以下の 10 秒検索で比較することにした。

実行コマンド（rshogi）:

```bash
perf stat -x, -e cycles,instructions,branches,branch-misses,cache-references,cache-misses,L1-dcache-load-misses bash -lc '
  tmpout=$(mktemp)
  tmpfifo=$(mktemp -u)
  mkfifo "$tmpfifo"
  (
    printf "usi\nsetoption name Threads value 1\nsetoption name USI_Hash value 256\nsetoption name EvalFile value /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin\nsetoption name LS_BUCKET_MODE value progress8kpabs\nsetoption name LS_PROGRESS_COEFF value /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin\nisready\nposition startpos\ngo movetime 10000\n"
    while ! grep -q "^bestmove" "$tmpout" 2>/dev/null; do sleep 0.1; done
    printf "quit\n"
  ) > "$tmpfifo" &
  timeout 20 taskset -c 4 /mnt/nvme1/development/rshogi/target/release/rshogi-usi < "$tmpfifo" > "$tmpout" 2>&1
  wait $! 2>/dev/null || true
  grep "^info depth " "$tmpout" | tail -1
  rm -f "$tmpout" "$tmpfifo"
'
```

実行コマンド（YaneuraOu）:

```bash
perf stat -x, -e cycles,instructions,branches,branch-misses,cache-references,cache-misses,L1-dcache-load-misses bash -lc '
  tmpout=$(mktemp)
  tmpfifo=$(mktemp -u)
  mkfifo "$tmpfifo"
  (
    printf "usi\nsetoption name Threads value 1\nsetoption name USI_Hash value 256\nsetoption name BookFile value no_book\nsetoption name LS_BUCKET_MODE value progress8kpabs\nsetoption name LS_PROGRESS_COEFF value /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin\nisready\nposition startpos\ngo movetime 10000\n"
    while ! grep -q "^bestmove" "$tmpout" 2>/dev/null; do sleep 0.1; done
    printf "quit\n"
  ) > "$tmpfifo" &
  timeout 20 taskset -c 4 /mnt/nvme1/development/YaneuraOu/source/YaneuraOu-sfnnwop1536-v2 < "$tmpfifo" > "$tmpout" 2>&1
  wait $! 2>/dev/null || true
  grep "^info depth " "$tmpout" | tail -1
  rm -f "$tmpout" "$tmpfifo"
'
```

検索結果:

| engine | final info line |
| --- | --- |
| rshogi | `info depth 32 ... nodes 6081637 time 10000 nps 608163 ...` |
| YO | `info depth 32 ... nodes 6702606 time 10000 nps 670260 ...` |

`perf stat` 生値:

| metric | rshogi | YO |
| --- | ---: | ---: |
| cycles | 52,815,867,426 | 50,965,535,721 |
| instructions | 104,730,637,161 | 92,659,914,577 |
| IPC | 1.98 | 1.82 |
| branches | 11,180,112,617 | 11,514,444,309 |
| branch-misses | 298,751,535 | 293,441,342 |
| branch-miss rate | 2.67% | 2.55% |
| cache-references | 13,766,413,999 | 15,074,797,576 |
| cache-misses | 949,180,176 | 1,007,831,896 |
| cache-miss rate | 6.89% | 6.69% |
| L1-dcache-load-misses | 7,057,180,999 | 8,700,277,498 |

ノード正規化:

| metric | rshogi | YO | ratio (r/yo) |
| --- | ---: | ---: | ---: |
| cycles / node | 8,684.5 | 7,603.8 | 1.142 |
| instructions / node | 17,220.8 | 13,824.5 | 1.246 |
| branches / node | 1,838.3 | 1,717.9 | 1.070 |
| cache-misses / kNodes | 156,073.1 | 150,364.2 | 1.038 |
| L1-dcache-load-misses / kNodes | 1,160,408.1 | 1,298,044.0 | 0.894 |

ここで分かった事実:

- この条件では YO の NPS は rshogi より約 `10.2%` 高い
- 差の主因は IPC 低下ではなく、rshogi の `instructions / node` 増加
- `cycles / node` も rshogi が約 `14.2%` 高い
- cache miss 率は大差なく、L1 miss / node はむしろ YO の方が高い
- よって、少なくともこの時点では「メモリミス主導」より「余計な命令数主導」を疑うのが妥当

補足:

- `perf stat -p <pid>` で初期化後 attach する方式も試したが、この環境では `<not counted>` になり安定しなかったため採用しなかった

### 3. `perf record` 比較

まず stripped バイナリのまま `perf record` を取ると、`__memmove_avx_unaligned_erms` などは見えるが rshogi 本体の関数名がほぼ解決できなかった。
そのため、rshogi のみ debug info 付きで再ビルドした。

実行コマンド:

```bash
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
file target/release/rshogi-usi
```

結果:

- `target/release/rshogi-usi: ... with debug_info, not stripped`

その後、10 秒検索条件で `perf record` を取得した。

実行コマンド:

```bash
perf record -F 997 -o /tmp/rshogi-movetime10-debug.data bash -lc '
  tmpout=$(mktemp)
  tmpfifo=$(mktemp -u)
  mkfifo "$tmpfifo"
  (
    printf "usi\nsetoption name Threads value 1\nsetoption name USI_Hash value 256\nsetoption name EvalFile value /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin\nsetoption name LS_BUCKET_MODE value progress8kpabs\nsetoption name LS_PROGRESS_COEFF value /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin\nisready\nposition startpos\ngo movetime 10000\n"
    while ! grep -q "^bestmove" "$tmpout" 2>/dev/null; do sleep 0.1; done
    printf "quit\n"
  ) > "$tmpfifo" &
  timeout 20 taskset -c 4 /mnt/nvme1/development/rshogi/target/release/rshogi-usi < "$tmpfifo" > "$tmpout" 2>&1
  wait $! 2>/dev/null || true
  grep "^info depth " "$tmpout" | tail -1
  rm -f "$tmpout" "$tmpfifo"
'
perf report -i /tmp/rshogi-movetime10-debug.data --stdio --no-children -g none --percent-limit 0.5
```

主なホットスポット:

| overhead | symbol |
| ---: | --- |
| 12.99% | `FeatureTransformerLayerStacks::update_accumulator_with_cache` |
| 12.68% | `LayerStackBucket::propagate` |
| 6.90% | `SearchWorker::search_node` |
| 6.85% | `__memmove_avx_unaligned_erms` |
| 5.71% | `MovePicker::next_move` |
| 3.36% | `NetworkLayerStacks::evaluate_with_bucket` |
| 2.95% | `Position::do_move_with_prefetch` |
| 2.28% | `decode_leb128_all_i16` |
| 2.02% | `refresh_perspective_with_cache` |
| 1.67% | `partial_insertion_sort` |

annotate で見えた事実:

- `update_accumulator_with_cache` 内では `curr.copy_from_slice(prev)` に加えて、`collect_changed_indices()` の戻り値展開まわりで大きめのスタックコピーが発生している
- `collect_changed_indices()` は `ChangedFeatures = (IndexList<MAX_CHANGED_FEATURES>, IndexList<MAX_CHANGED_FEATURES>)` を値で返しており、この ABI が余計な move/copy を生んでいる可能性が高い
- `LayerStackBucket::propagate` は AVX2 化されているが、L1 後の `SqrClippedReLU + pack` 部分にかなり命令が集中している
- `MovePicker::next_move` は約 `0x1388` バイトの大きいスタックフレームを持ち、捕獲手スコアリングと `partial_insertion_sort` が見えている

この時点の暫定解釈:

- いちばん疑わしいのは rshogi の NNUE 差分更新パスでの「値返し/コピー起因の instruction 増」
- 2 番目は `LayerStackBucket::propagate` の中間変換コスト
- cache miss 率だけでは説明しにくく、`instructions / node` 増と整合する

### 4. 仮説A検証: `collect_changed_indices()` の値返しを除去

静的比較で、YO は changed indices を caller 側の配列に直接書き込んでいる一方、rshogi は
`ChangedFeatures = (IndexList<...>, IndexList<...>)` を値で返していた。
`perf annotate` でも `update_accumulator_with_cache` 内で大きなスタック move/copy が見えていたため、
LayerStacks 用 FT のホットパスだけを最小修正した。

修正内容:

- 対象: `crates/rshogi-core/src/nnue/feature_transformer_layer_stacks.rs`
- 変更点:
  - `update_accumulator`
  - `update_accumulator_with_cache`
  - `forward_update_incremental`
- 上記 3 箇所で `HalfKA_hm_FeatureSet::collect_changed_indices()` をやめ、
  caller 側で `IndexList` を確保して `<HalfKA_hm as Feature>::append_changed_indices(...)` を直接呼ぶように変更

検証コマンド:

```bash
cargo fmt
cargo clippy --fix --allow-dirty --tests
cargo test
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
```

結果:

- `cargo fmt`: 成功
- `cargo clippy --fix --allow-dirty --tests`: 成功
- `cargo test`: 成功（主要集計: `734 passed, 0 failed, 7 ignored` for `rshogi_core`; `6 passed` for `rshogi_usi`; `69 passed` for `tools`）
- `cargo build --release --bin rshogi-usi`: 成功

再計測（depth 20, rshogi 3回）:

| run | final info line |
| --- | --- |
| 1 | `info depth 20 ... nodes 383432 time 616 nps 622454 ...` |
| 2 | `info depth 20 ... nodes 383432 time 625 nps 613491 ...` |
| 3 | `info depth 20 ... nodes 383432 time 606 nps 632726 ...` |

集計:

- 変更前 rshogi 平均 NPS: `602,097`
- 変更後 rshogi 平均 NPS: `622,890`
- 改善率: `+3.45%`

再計測（10 秒 `perf stat`, rshogi のみ）:

| metric | before | after | ratio (after/before) |
| --- | ---: | ---: | ---: |
| nodes | 6,081,637 | 6,274,119 | 1.032 |
| nps | 608,163 | 627,474 | 1.032 |
| cycles | 52,815,867,426 | 49,381,725,412 | 0.935 |
| instructions | 104,730,637,161 | 104,436,166,685 | 0.997 |
| IPC | 1.98 | 2.11 | 1.066 |
| branch-miss rate | 2.67% | 2.31% | - |

ノード正規化:

| metric | before | after | ratio (after/before) |
| --- | ---: | ---: | ---: |
| cycles / node | 8,684.5 | 7,870.7 | 0.906 |
| instructions / node | 17,220.8 | 16,645.6 | 0.967 |
| branches / node | 1,838.3 | 1,730.3 | 0.941 |

`perf record` 再取得結果:

| overhead | before | after |
| --- | ---: | ---: |
| `LayerStackBucket::propagate` | 12.68% | 13.35% |
| `update_accumulator_with_cache` | 12.99% | 12.25% |
| `__memmove_avx_unaligned_erms` | 6.85% | 6.89% |
| `MovePicker::next_move` | 5.71% | 5.88% |

ここで分かった事実:

- `collect_changed_indices()` の値返し除去は実際に効いた
- 効果量はこの負荷環境で `+3.45% NPS`
- `instructions / node` は約 `3.3%` 減少、`cycles / node` は約 `9.4%` 減少
- ただし NPS 差を単独で埋めきるには足りず、依然として `LayerStackBucket::propagate` が最大ホットスポット
- 変更後の 10 秒計測では、YO `670,260 nps` に対して rshogi `627,474 nps` で、差は約 `6.8%` まで縮小

## 暫定メモ

- 引継ぎ資料と既存メモから、`AccumulatorCaches + MAX_DEPTH=4` の有無そのものが主因である可能性は低い
- まずは NNUE 推論を含む 1 ノード当たりコスト差を優先して切り分ける
- 現時点の優先候補:
  - `LayerStackBucket::propagate()` の `SqrClippedReLU + pack` 部分の命令数を詰める
  - `MovePicker::next_move()` の大きいスタックフレームと sort コストを再確認する
  - 必要なら `network_layer_stacks` / `layers` の既存 SIMD 部品へ寄せて、YO の `ClippedReLUExplicit` / `SqrClippedReLU` に近いコード形へ再編する
