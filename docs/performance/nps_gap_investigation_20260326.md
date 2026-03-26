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

### 5. 仮説B検証: `LayerStackBucket::propagate()` を既存 SIMD 部品へ寄せる

別エージェントによって `crates/rshogi-core/src/nnue/layer_stacks.rs` が一度 `git checkout` されていたため、
まず候補パッチを再適用してから検証した。

試した変更:

- 対象: `crates/rshogi-core/src/nnue/layer_stacks.rs`
- 変更点:
  - L1 後半の ReLU を scalar loop ではなく `ClippedReLU::<LAYER_STACK_L1_OUT>::propagate()` に置換
  - L2 後の ReLU を scalar loop ではなく `ClippedReLU::<NNUE_PYTORCH_L3>::propagate()` に置換
  - L1 の `SqrClippedReLU` 相当を `l1_sqr_clipped_relu()` として追加し、YO の `SqrClippedReLU` に近い SSE2 実装を試した

検証途中で分かったこと:

- 追加テストで `8192 -> 128` の不一致が出た
- 原因は `_mm_packus_epi16` を使っていたことで、YO は `_mm_packs_epi16` の signed saturate で `127` 上限を守っている
- ここを修正後、`cargo fmt` / `cargo clippy --fix --allow-dirty --tests` / `cargo test` は通過した

検証コマンド:

```bash
cargo fmt
cargo clippy --fix --allow-dirty --tests
cargo test
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
```

再計測（depth 20, rshogi 3回）:

| run | final info line |
| --- | --- |
| 1 | `info depth 20 ... nodes 383432 time 638 nps 600990 ...` |
| 2 | `info depth 20 ... nodes 383432 time 614 nps 624482 ...` |
| 3 | `info depth 20 ... nodes 383432 time 618 nps 620440 ...` |

集計:

- section 4 後の rshogi 平均 NPS: `622,890`
- この候補適用時の平均 NPS: `615,304`
- 比率: `0.9878x`（約 `-1.22%`）

再計測（10 秒 `perf stat`, rshogi のみ）:

検索結果:

| engine | final info line |
| --- | --- |
| rshogi | `info depth 32 ... nodes 6061876 time 9999 nps 606248 ...` |

`perf stat` 生値:

| metric | value |
| --- | ---: |
| cycles | 49,264,318,870 |
| instructions | 100,773,646,926 |
| IPC | 2.05 |
| branches | 10,503,428,840 |
| branch-misses | 245,606,784 |
| branch-miss rate | 2.34% |
| cache-references | 13,965,134,048 |
| cache-misses | 901,436,895 |
| cache-miss rate | 6.45% |
| L1-dcache-load-misses | 6,977,939,631 |

section 4 後との比較:

| metric | section 4後 | この候補 | ratio (candidate/section4) |
| --- | ---: | ---: | ---: |
| nodes | 6,274,119 | 6,061,876 | 0.966 |
| nps | 627,474 | 606,248 | 0.966 |
| cycles | 49,381,725,412 | 49,264,318,870 | 0.998 |
| instructions | 104,436,166,685 | 100,773,646,926 | 0.965 |
| IPC | 2.11 | 2.05 | 0.972 |
| branch-miss rate | 2.31% | 2.34% | - |

ノード正規化:

| metric | section 4後 | この候補 | ratio (candidate/section4) |
| --- | ---: | ---: | ---: |
| cycles / node | 7,870.7 | 8,126.9 | 1.033 |
| instructions / node | 16,645.6 | 16,624.2 | 0.999 |
| branches / node | 1,730.3 | 1,732.7 | 1.001 |
| cache-misses / kNodes | 155,552.1 | 148,705.9 | 0.956 |
| L1-dcache-load-misses / kNodes | 1,146,644.9 | 1,151,118.8 | 1.004 |

`perf record` 再取得結果:

| overhead | section 4後 | この候補 |
| --- | ---: | ---: |
| `LayerStackBucket::propagate` | 13.35% | 13.87% |
| `update_accumulator_with_cache` | 12.25% | 11.24% |
| `MovePicker::next_move` | 5.88% | 6.24% |
| `__memmove_avx_unaligned_erms` | 6.89% | 5.90% |

ここで分かった事実:

- この候補は total instructions と cache-misses を少し減らしたが、`cycles / node` は悪化した
- `instructions / node` はほぼ横ばいで、NPS 低下を説明するのは主に `cycles / node` 増
- `LayerStackBucket::propagate` の割合はむしろ上がっており、generic `ClippedReLU` 合成だけでは勝てない
- 少なくともこの負荷環境では、YO に寄せるなら generic 部品の組み合わせではなく、
  aligned buffer と explicit kernel を含めたより忠実な実装が必要

判断:

- この候補は退行と判断し、`crates/rshogi-core/src/nnue/layer_stacks.rs` は検証後に元実装へ戻した
- 差し戻し後に `cargo fmt` / `cargo clippy --fix --allow-dirty --tests` / `cargo test` を再実行し、通過を確認した

補足:

- `.cargo/config.toml` で `target-cpu=native` が有効
- したがって少なくともビルド設定上は、Rust 側もホスト CPU 向け最適化を有効化している

### 6. 仮説C検証: `MovePicker::next_move()` の大スタックフレーム削減

`perf annotate` では、`MovePicker::next_move()` の関数入口が
`sub $0x1000; sub $0x388` となっており、約 `0x1388` バイトの大きなスタックフレームを確保していた。
静的確認すると、`generate_all_legal_moves=true` の `QuietInit` / `EvasionInit` で
ローカル `ExtMoveBuffer` を確保しているのが原因だった。

修正内容:

- 対象: `crates/rshogi-core/src/search/movepicker.rs`
- 変更点:
  - `QuietInit` の `generate_all_legal_moves` 分岐で、ローカル `ExtMoveBuffer` を廃止
  - 既存の `self.moves` に対して `GenType::QuietsAll` を直接 append するよう変更
  - `EvasionInit` でもローカル `ExtMoveBuffer` を廃止し、`GenType::EvasionsAll` を直接 `self.moves` に生成

狙い:

- `next_move()` のスタックフレームを縮小し、stack spill / clear / memcpy を減らす
- `MovePicker::next_move()` 自体のホットパスコストを下げる

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
| 1 | `info depth 20 ... nodes 383432 time 636 nps 602880 ...` |
| 2 | `info depth 20 ... nodes 383432 time 631 nps 607657 ...` |
| 3 | `info depth 20 ... nodes 383432 time 628 nps 610560 ...` |

集計:

- 平均 NPS: `607,032`

再計測（10 秒 `perf stat`, rshogi のみ, 2回）:

検索結果:

| run | final info line |
| --- | --- |
| 1 | `info depth 32 ... nodes 6180384 time 10000 nps 618038 ...` |
| 2 | `info depth 32 ... nodes 6137941 time 10000 nps 613794 ...` |

`perf stat` 生値:

| metric | run 1 | run 2 |
| --- | ---: | ---: |
| cycles | 49,549,457,149 | 49,396,219,393 |
| instructions | 102,858,735,992 | 102,168,127,791 |
| IPC | 2.08 | 2.07 |
| branches | 10,671,334,425 | 10,635,725,641 |
| branch-miss rate | 2.29% | 2.29% |
| cache-misses | 888,740,538 | 866,393,523 |
| L1-dcache-load-misses | 7,058,143,552 | 7,002,503,110 |

ノード正規化:

| metric | run 1 | run 2 | 2-run average |
| --- | ---: | ---: | ---: |
| cycles / node | 8,017.2 | 8,047.7 | 8,032.4 |
| instructions / node | 16,642.8 | 16,645.3 | 16,644.1 |
| branches / node | 1,726.6 | 1,732.8 | 1,729.7 |
| cache-misses / kNodes | 143,800.2 | 141,153.8 | 142,479.9 |
| L1-dcache-load-misses / kNodes | 1,142,023.5 | 1,140,855.4 | 1,141,439.6 |

`perf record` 再取得結果:

| overhead | section 4後 | この候補 |
| --- | ---: | ---: |
| `MovePicker::next_move` | 5.88% | 6.46% |
| `partial_insertion_sort` | 2.02% | 1.86% |
| `LayerStackBucket::propagate` | 13.35% | 13.52% |
| `update_accumulator_with_cache` | 12.25% | 10.95% |

annotate で見えた事実:

- `MovePicker::next_move()` の関数入口は `sub $0x1388` 相当から `sub $0xc8` まで縮小した
- つまりローカル `ExtMoveBuffer` 自体は確かに大きなスタックフレームの主因だった

ここで分かった事実:

- 大スタックフレームの削減自体は達成した
- しかしこの負荷環境では NPS 改善は確認できず、`depth 20` / `perf stat` ともに section 4 後より弱い
- `instructions / node` はほぼ横ばいで、良くも悪くも主効果にはなっていない
- よって、この候補は「構造改善は正しいが、現時点の実測では採用根拠が弱い」と判断する

判断:

- この候補は性能改善としては採用せず、`crates/rshogi-core/src/search/movepicker.rs` は検証後に元実装へ戻す
- ただし「ローカル `ExtMoveBuffer` が大スタックの主因だった」という知見自体は有効
- 将来 `MovePicker` を再度詰めるなら、stack frame 削減だけでなく
  `partial_insertion_sort` / quiet scoring / movegen 呼び出し回数まで一体で見直す必要がある

### 7. search-only `perf` 手順の確立と再比較

ここまでの `perf stat` / `perf record` は、USI startup と `isready` 中の NNUE ロードも含んでいた。
`__memmove_avx_unaligned_erms` の caller を `perf script` で見たところ、wrapper 計測では
`FeatureTransformerLayerStacks::read_leb128()` が多数混入していた。

そのため、以後は `perf stat -D -1 --control ...` / `perf record -D -1 --control ...` を使い、
エンジンは起動するがカウンタは無効のまま待機し、`readyok` 後に `enable`、
`bestmove` 直後に `disable` する方式へ切り替えた。

確認した事実:

- `perf stat -p <pid>` の attach は、この環境では依然 `<not counted>` になり不採用
- `perf record -p <pid>` の attach は動作する
- ただし最も安定したのは `--control fd:...` による start/stop 制御

代表コマンド（rshogi, search-only `perf stat`）:

```bash
perf stat -D -1 --control fd:9,8 -x, \
  -e cycles,instructions,branches,branch-misses,cache-references,cache-misses,L1-dcache-load-misses \
  -- taskset -c 4 /mnt/nvme1/development/rshogi/target/release/rshogi-usi
```

制御手順:

1. `usi`
2. `setoption name Threads value 1`
3. `setoption name USI_Hash value 256`
4. `setoption name EvalFile value /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin`
5. `setoption name LS_BUCKET_MODE value progress8kpabs`
6. `setoption name LS_PROGRESS_COEFF value /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin`
7. `isready` → `readyok`
8. control FD に `enable`
9. `position startpos`
10. `go movetime 10000`
11. `bestmove` 受信後に control FD へ `disable`

YO 側は `EvalFile` の代わりに `BookFile=no_book` を指定し、それ以外は同条件で実施した。

search-only 比較結果:

| engine | final info line |
| --- | --- |
| rshogi | `info depth 31 ... nodes 5232853 time 9999 nps 523337 ...` |
| YO | `info depth 32 ... nodes 5748249 time 10000 nps 574824 ...` |

`perf stat` 生値:

| metric | rshogi | YO |
| --- | ---: | ---: |
| cycles | 39,941,937,647 | 39,507,750,291 |
| instructions | 82,685,636,629 | 74,250,158,904 |
| IPC | 2.07 | 1.88 |
| branches | 8,260,121,155 | 8,758,813,192 |
| branch-miss rate | 1.91% | 1.51% |
| cache-references | 12,051,990,142 | 12,886,556,218 |
| cache-misses | 750,457,289 | 871,774,345 |
| cache-miss rate | 6.23% | 6.76% |
| L1-dcache-load-misses | 6,026,749,815 | 7,483,304,254 |

ノード正規化:

| metric | rshogi | YO | ratio (r/yo) |
| --- | ---: | ---: | ---: |
| cycles / node | 7,632.9 | 6,873.0 | 1.111 |
| instructions / node | 15,801.3 | 12,917.0 | 1.223 |
| branches / node | 1,578.5 | 1,523.7 | 1.036 |
| cache-misses / kNodes | 143,412.6 | 151,659.1 | 0.946 |
| L1-dcache-load-misses / kNodes | 1,151,714.0 | 1,301,840.7 | 0.884 |

ここで分かった事実:

- startup を除いても、YO は rshogi より約 `9.8%` 高速
- 差の主因は引き続き `instructions / node` 増加で、search-only 条件では約 `22.3%` 多い
- `cycles / node` も rshogi が約 `11.1%` 高い
- cache miss は依然として主因ではなく、`cache-misses / kNodes` と `L1-misses / kNodes` はむしろ rshogi の方が低い
- したがって「startup 由来ノイズを除いても、命令数主導」という結論は維持される

### 8. search-only `perf record` と `memmove` caller の再確認

`perf record` も同じ `--control` 方式で search-only 取得した。

実行コマンド:

```bash
perf record -D -1 --control fd:9,8 -F 997 -g --call-graph dwarf,4096 \
  -o /tmp/rshogi-searchonly.data \
  -- taskset -c 4 /mnt/nvme1/development/rshogi/target/release/rshogi-usi
```

結果:

- final info: `info depth 30 ... nodes 5038321 time 9999 nps 503882 ...`
- `perf record: Captured and wrote 41.111 MB /tmp/rshogi-searchonly.data (9903 samples)`

主なホットスポット:

| overhead | symbol |
| ---: | --- |
| 14.42% | `LayerStackBucket::propagate` |
| 12.83% | `FeatureTransformerLayerStacks::update_accumulator_with_cache` |
| 7.37% | `SearchWorker::search_node` |
| 7.13% | `__memmove_avx_unaligned_erms` |
| 6.65% | `MovePicker::next_move` |
| 4.27% | `NetworkLayerStacks::evaluate_with_bucket` |
| 3.30% | `Position::do_move_with_prefetch` |
| 2.45% | `refresh_perspective_with_cache` |
| 2.16% | `Position::update_repetition_info` |
| 2.08% | `check_move_mate` |
| 1.75% | `partial_insertion_sort` |

`perf script --no-inline` で `__memmove_avx_unaligned_erms` を含むサンプルを集計した結果:

| immediate parent (resolvable only) | samples |
| --- | ---: |
| `<unknown>` | 609 |
| `FeatureTransformerLayerStacks::update_accumulator_with_cache` | 24 |
| `LayerStackBucket::propagate` | 24 |
| `SearchWorker::search_node` | 19 |
| `refresh_perspective_with_cache` | 10 |

補足:

- DWARF unwind / `addr2line` 解決はまだ不完全で、caller の大半は `<unknown>` に落ちる
- それでも、search-only でも `memmove` は startup ではなく探索中のコピーとして残っていると判断できる
- `addr2line -Cfpie target/release/rshogi-usi 0x2b3be6` は
  `FeatureTransformerLayerStacks::update_accumulator_with_cache` 内の
  [`curr.copy_from_slice(prev)`](/mnt/nvme1/development/rshogi/crates/rshogi-core/src/nnue/feature_transformer_layer_stacks.rs#L246)
  に対応していた
- `perf annotate` 上、`SearchWorker::search_node` の入口は依然として
  `sub $0x1000; sub $0x718` の大スタックフレームを持つ

### 9. 測定系の注意: `benchmark` は現状 NNUE 調査に不適

既存の `benchmark --internal` / `benchmark --engine` も試したが、
現状の実装では NNUE 調査にそのまま使えないことが分かった。

根拠:

- `crates/tools/src/runner/internal.rs` は `setup_eval()` で常に `MaterialLevel` を設定する
- `crates/tools/src/runner/usi.rs` も常に `setoption name MaterialLevel value ...` を送る
- 一方 `crates/rshogi-core/src/nnue/network.rs` は `material::is_material_enabled()` のとき
  NNUE ではなく material eval を返す

実際に `benchmark --internal --reuse-search --nnue-file ...` を回すと
`NNUE initialized` と表示される一方で、`perf report` 上位は
`eval_lv7_like` / `direction_of` になり、NNUE ホットスポットは出なかった。

判断:

- 今回の NNUE NPS 差異調査では `benchmark` は使わず、USI + `perf --control` を正式手順にする
- `benchmark` 自体の eval 切り替えは別タスク

### 10. 仮説D検証: FT の copy path を explicit SIMD copy に置換

次候補として、`FeatureTransformerLayerStacks` の copy path を
AVX2/SSE2 の explicit load/store に置換する案を試した。

対象:

- `refresh_accumulator()` の `biases` コピー
- `update_accumulator()` の reset / non-reset 両パス
- `update_accumulator_with_cache()` の non-reset パス
- `forward_update_incremental()` の source accumulator 複製

狙い:

- search-only `perf` で親として見えている `curr.copy_from_slice(prev)` を明示 SIMD copy に置換し、
  `update_accumulator_with_cache` の copy コストを下げる

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
- `cargo test`: 成功
- `cargo build --release --bin rshogi-usi`: 成功

search-only `perf stat` 再計測（2回）:

| run | final info line |
| --- | --- |
| 1 | `info depth 30 ... nodes 4996963 time 9999 nps 499746 ...` |
| 2 | `info depth 30 ... nodes 5111706 time 10000 nps 511170 ...` |

`perf stat` 生値:

| metric | run 1 | run 2 |
| --- | ---: | ---: |
| cycles | 40,554,182,337 | 40,642,816,735 |
| instructions | 79,078,795,045 | 80,903,741,891 |
| IPC | 1.95 | 1.99 |
| branches | 7,908,300,786 | 8,086,794,225 |
| branch-miss rate | 1.92% | 1.92% |
| cache-misses | 761,018,796 | 757,184,539 |
| L1-dcache-load-misses | 5,729,944,644 | 5,838,542,491 |

ノード正規化:

| metric | run 1 | run 2 | 2-run average |
| --- | ---: | ---: | ---: |
| NPS | 499,746 | 511,170 | 505,458 |
| cycles / node | 8,115.8 | 7,950.9 | 8,033.3 |
| instructions / node | 15,825.4 | 15,827.2 | 15,826.3 |
| branches / node | 1,582.6 | 1,582.0 | 1,582.3 |
| cache-misses / kNodes | 152,296.3 | 148,127.6 | 150,211.9 |
| L1-dcache-load-misses / kNodes | 1,146,685.4 | 1,142,190.6 | 1,144,438.0 |

section 7 の search-only baseline との比較:

- 平均 NPS: `523,337 -> 505,458` (`-3.42%`)
- `cycles / node`: `7,632.9 -> 8,033.3` (`+5.25%`)
- `instructions / node`: `15,801.3 -> 15,826.3` (`+0.16%`)

判断:

- この候補は明確に退行
- `crates/rshogi-core/src/nnue/feature_transformer_layer_stacks.rs` は検証後に差し戻した
- 少なくとも Zen 3 + この build では、`copy_from_slice` を explicit SIMD copy にするだけでは改善しない
- したがって次に触るべきなのは copy primitive そのものではなく、
  `LayerStackBucket::propagate` か、`search_node` / history 更新の大スタック・大コピー経路

### 11. LayerStack 専用 microbench の追加

search-only `perf --control` は正確だが、`LayerStackBucket::propagate()` の候補を試すには
反復が重い。そこで `crates/tools/src/bin/bench_nnue_eval.rs` を拡張し、
LayerStacks 専用の microbench を追加した。

実装内容:

- `--mode full`（既存）
- `--mode layer-stack-propagate`
- `--mode layer-stack-eval`
- `--ls-bucket-mode` と `--ls-ply-bounds` を追加
- `progress8kpabs` では既存の `--ls-progress-coeff <progress.bin>` を再利用
- 固定局面から実際に `refresh_accumulator` して、`propagate` 用 transformed 入力 /
  `evaluate_with_bucket` 用 accumulator を前計算する
- 既存の固定局面 1 件に不正 SFEN（total pawn=21）が含まれていたため、valid SFEN に差し替えた

理由:

- 既存の `bench_nnue_eval` は `refresh` と full `evaluate_only` の粒度で、
  `LayerStackBucket::propagate()` を直接測れなかった
- 既存の `benchmark` 系は `MaterialLevel` を触るため NNUE 調査には不向きだった
- `propagate` 候補の一次判定を microbench で行い、通ったものだけ search-only `perf` に進める

検証コマンド:

```bash
cargo fmt
cargo clippy --fix --allow-dirty --tests
cargo test
cargo build --release --bin bench_nnue_eval
taskset -c 4 ./target/release/bench_nnue_eval \
  --mode layer-stack-propagate \
  --nnue-file /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin \
  --iterations 1000000 \
  --warmup 100000 \
  --ls-bucket-mode progress8kpabs \
  --ls-progress-coeff /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin
taskset -c 4 ./target/release/bench_nnue_eval \
  --mode layer-stack-eval \
  --nnue-file /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin \
  --iterations 1000000 \
  --warmup 100000 \
  --ls-bucket-mode progress8kpabs \
  --ls-progress-coeff /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin
```

結果（最終 tree, 直列実行）:

- `cargo fmt`: 成功
- `cargo clippy --fix --allow-dirty --tests`: 成功
- `cargo test`: 成功
- `cargo build --release --bin bench_nnue_eval`: 成功
- `layer-stack-propagate`: `300.4 ns/op` (`3,328,571 ops/sec`)
- `layer-stack-eval`: `349.5 ns/op` (`2,861,377 ops/sec`)
- データセット bucket 分布: `0:3, 6:1, 7:1`

注意:

- microbench を同じ固定コアで並列に走らせると値が大きく崩れることを確認した
- このため、以後の `bench_nnue_eval` は必ず直列で使う

### 12. 仮説E検証: YO 寄り explicit helper で `propagate()` を絞って最適化

`LayerStackBucket::propagate()` に対して、YO の `ClippedReLUExplicit<16>` /
`SqrClippedReLU<16>` / `ClippedReLUExplicit<32>` に寄せた helper を一時的に実装し、
L1 後半と L2 後半の活性化だけを explicit kernel 化する案を試した。

狙い:

- 前回の generic `ClippedReLU` 合成ではなく、YO と同じ粒度で中間活性化だけを切り出す
- search-only `perf` へ進む前に、新設した microbench で一次判定する

microbench で使った直列 baseline（候補適用前）:

- `layer-stack-propagate`: `310.6 ns/op`, `296.4 ns/op`（平均 `303.5 ns/op`）
- `layer-stack-eval`: `413.7 ns/op`, `357.1 ns/op`（平均 `385.4 ns/op`）

候補適用後の代表結果（直列）:

- `layer-stack-propagate`: `314.4 ns/op`
- `layer-stack-eval`: `364.1 ns/op`

分かった事実:

- `layer-stack-eval` 全体では少し改善して見える run もあった
- ただし最大ホットパスである `layer-stack-propagate` 単体は改善していない
- 直列 run だけで見ると `propagate` は baseline 平均 `303.5 ns/op` に対して
  候補 `314.4 ns/op` で、少なくとも明確な改善ではない
- 並列 run は相互干渉で悪化幅が不安定だったため、判断材料から除外した

判断:

- この候補は microbench の一次ゲートを通過しなかった
- `crates/rshogi-core/src/nnue/layer_stacks.rs` は検証後に差し戻した
- したがって search-only `perf` / NPS の本検証には進めていない
- 新設した microbench は今後も `propagate` 候補の一次判定に使う
- 次に `LayerStackBucket::propagate()` を触るなら、活性化 helper 単体より
  バッファ配置と `fc_1` への受け渡しを含めた構造差を見た方が良い

## 暫定メモ

- 引継ぎ資料と既存メモから、`AccumulatorCaches + MAX_DEPTH=4` の有無そのものが主因である可能性は低い
- まずは NNUE 推論を含む 1 ノード当たりコスト差を優先して切り分ける
- 現時点の優先候補:
  - `bench_nnue_eval --mode layer-stack-propagate` / `layer-stack-eval` / `layer-stack-refresh-cache` / `layer-stack-update-cache` を LayerStacks 候補の一次ゲートとして使う
  - `LayerStackBucket::propagate()` を再度触るなら、generic `ClippedReLU` 合成ではなく YO の aligned / explicit 実装へより忠実に寄せる
  - `SearchWorker::search_node` / history 更新側の大スタック・大コピー経路を静的比較と `addr2line` で詰める
  - `FeatureTransformerLayerStacks::update_accumulator_with_cache` は microbench だけでは採用判断せず、section 7 の search-only A/B で必ず再確認する

### 13. 仮説F検証: changed indices の両視点 1 パス化

`HalfKA_hm::append_changed_indices()` を視点ごとに 2 回呼んでいるのが
YO の `RawFeatures::AppendChangedIndices()` と違うため、
`update_accumulator()` / `update_accumulator_with_cache()` /
`forward_update_incremental()` で両視点の changed indices を 1 パスで構築する案を試した。

実施内容:

- `HalfKA_hm::append_changed_indices_both()` を追加
- `FeatureTransformerLayerStacks` 側の 3 箇所をこの helper 呼び出しに置換
- helper と既存 single-perspective 実装の一致を unit test で固定

検証コマンド:

```bash
cargo fmt
cargo clippy --fix --allow-dirty --tests
cargo test
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
git worktree add --detach /tmp/rshogi-head-e8ca06b3 HEAD
CARGO_TARGET_DIR=/tmp/rshogi-head-e8ca06b3-target-native \
RUSTFLAGS='-C target-cpu=native' \
CARGO_PROFILE_RELEASE_DEBUG=2 \
CARGO_PROFILE_RELEASE_STRIP=none \
cargo build --release --bin rshogi-usi
```

比較方法:

- section 7 と同じ search-only `perf --control` 手順を使用
- baseline は `/tmp/rshogi-head-e8ca06b3-target-native/release/rshogi-usi`
- candidate は `target/release/rshogi-usi`
- 順序依存のノイズを見るため、`head_native -> current` と `current -> head_native` の 2 パターンを実施

注意:

- 最初に temp worktree をそのまま build したところ、current tree の `.cargo/config.toml`
  (`-C target-cpu=native`) が存在しないため generic codegen になり、
  NPS が約半分まで落ちて比較不能だった
- その run は破棄し、`RUSTFLAGS='-C target-cpu=native'` 付き build にやり直した

search-only `perf stat` 結果:

| order | engine | final nps | cycles / node | instructions / node |
| --- | --- | ---: | ---: | ---: |
| 1 | head_native | `513,656` | `7,815.9` | `15,821.0` |
| 1 | current | `501,946` | `7,982.9` | `15,812.1` |
| 2 | current | `498,752` | `8,060.9` | `15,805.5` |
| 2 | head_native | `489,978` | `8,131.4` | `15,801.0` |

平均:

| engine | avg nps | avg cycles / node | avg instructions / node |
| --- | ---: | ---: | ---: |
| head_native | `501,817` | `7,973.7` | `15,811.0` |
| current | `500,349` | `8,021.9` | `15,808.8` |

分かった事実:

- 順序を入れ替えると優劣が反転し、run-to-run ノイズは少なくとも `~2%` 規模で存在する
- 2 run 平均では current は head_native 比で `-0.29%` とほぼフラット
- `instructions / node` は平均でほぼ一致しており、期待した命令数削減は確認できなかった
- `cycles / node` は平均で current がわずかに悪いが、ノイズ帯を明確には抜けていない

判断:

- この候補は「改善が確認できた」とは言えない
- コード複雑化に対してリターンが無いため、`append_changed_indices_both()` 案は差し戻した
- 最終 tree には残していない

### 14. 仮説G再検証: `MovePicker::next_move()` の一時 `ExtMoveBuffer` 除去

section 6 では旧比較手順で採用見送りにしていたが、search-only `perf --control`
同一 current tree baseline で再評価し直した。

修正内容:

- 対象: `crates/rshogi-core/src/search/movepicker.rs`
- `QuietInit` の `generate_all_legal_moves` でローカル `ExtMoveBuffer` を使わず、
  `self.moves` に `QuietsAll` を直接 append
- `EvasionInit` でも同様にローカル `ExtMoveBuffer` を廃止し、
  `self.moves` に `EvasionsAll` を直接生成

静的確認コマンド:

```bash
cp target/release/rshogi-usi /tmp/rshogi-usi-movepicker-baseline
nm -C /tmp/rshogi-usi-movepicker-baseline | rg "MovePicker::next_move"
nm -C target/release/rshogi-usi | rg "MovePicker::next_move"
objdump -d --start-address=0x26d310 --stop-address=0x26d340 /tmp/rshogi-usi-movepicker-baseline
objdump -d --start-address=0x26d2c0 --stop-address=0x26d2f0 target/release/rshogi-usi
```

分かった事実:

- `MovePicker::next_move()` の prologue は `sub $0x1000; sub $0x388` から `sub $0xc8` へ縮小
- つまり `generate_all_legal_moves=true` 分岐のローカル `ExtMoveBuffer` が
  大スタックフレームの主因だった

検証コマンド:

```bash
cargo fmt
cargo clippy --fix --allow-dirty --tests
cargo test
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
```

比較方法:

- section 7 と同じ search-only `perf --control` 手順を使用
- baseline は `/tmp/rshogi-usi-movepicker-baseline`
- candidate は `target/release/rshogi-usi`
- 順序依存ノイズを見るため `baseline -> candidate` と `candidate -> baseline` を実施

search-only `perf stat` 結果:

| order | engine | final nps | cycles / node | instructions / node |
| --- | --- | ---: | ---: | ---: |
| 1 | baseline | `503,951` | `8,097.5` | `15,814.9` |
| 1 | candidate | `532,860` | `7,600.6` | `15,773.7` |
| 2 | candidate | `519,645` | `7,846.0` | `15,791.8` |
| 2 | baseline | `498,716` | `8,098.0` | `15,822.9` |

平均:

| engine | avg nps | avg cycles / node | avg instructions / node |
| --- | ---: | ---: | ---: |
| baseline | `501,309.1` | `8,097.5` | `15,818.9` |
| candidate | `526,227.1` | `7,723.3` | `15,782.8` |

分かった事実:

- 2-order 平均で candidate は baseline 比 `+4.97%`
- `cycles / node` は約 `-4.62%` 改善
- `instructions / node` も約 `-0.23%` 改善
- 直近の YO 単発 search-only `529,934 nps` と比べると、
  この負荷環境での残差は 1% 未満の水準まで縮小した

判断:

- section 6 の旧判断は search-only A/B で上書きし、この変更は採用する
- `crates/rshogi-core/src/search/movepicker.rs` の現行差分として保持する

### 15. LayerStacks microbench 拡張と仮説H検証: no-reset 全体 copy fast path

`LayerStackBucket::propagate()` だけでは次の候補を切れないため、
`bench_nnue_eval` に LayerStacks の refresh / update 直叩きモードを追加した。

追加したモード:

- `--mode layer-stack-refresh-cache`
- `--mode layer-stack-update-cache`

補足:

- `layer-stack-update-cache` は既存 5 局面のうち、
  非玉合法手がある 4 局面だけを使う
- 裸玉局面は refresh ベンチには残し、update ベンチだけ除外する

追加後の baseline 測定コマンド:

```bash
cargo build --release --bin bench_nnue_eval
target/release/bench_nnue_eval \
  --nnue-file /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin \
  --mode layer-stack-refresh-cache \
  --ls-bucket-mode progress8kpabs \
  --ls-progress-coeff /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin \
  --warmup 10000 \
  --iterations 500000
target/release/bench_nnue_eval \
  --nnue-file /mnt/nvme1/development/bullet-shogi/checkpoints/v82/v82-300/quantised.bin \
  --mode layer-stack-update-cache \
  --ls-bucket-mode progress8kpabs \
  --ls-progress-coeff /mnt/nvme1/development/bullet-shogi/data/progress/nodchip_progress_e1_f1_cuda.bin \
  --warmup 10000 \
  --iterations 500000
```

baseline:

| mode | dataset buckets | ns/op |
| --- | --- | ---: |
| `layer-stack-refresh-cache` | `0:3, 6:1, 7:1` | `3193.2` |
| `layer-stack-update-cache` | `0:3, 6:1` | `233.1` |

次候補として、`update_accumulator()` / `update_accumulator_with_cache()` の
「両視点とも no-reset のとき、視点ごと 2 回ではなくアキュムレータ全体を 1 回 copy してから
差分適用する」fast path を試した。

microbench 再測定結果:

| mode | baseline ns/op | candidate ns/op | ratio |
| --- | ---: | ---: | ---: |
| `layer-stack-refresh-cache` | `3193.2` | `2961.1` | `0.927` |
| `layer-stack-update-cache` | `233.1` | `200.5` | `0.860` |

局所的には改善して見えたため、search-only A/B まで進めた。

検証コマンド:

```bash
cp target/release/rshogi-usi /tmp/rshogi-usi-updatecache-baseline
CARGO_PROFILE_RELEASE_DEBUG=2 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --bin rshogi-usi
```

比較方法:

- baseline は `/tmp/rshogi-usi-updatecache-baseline`
- candidate は `target/release/rshogi-usi`
- section 7 と同じ search-only `perf --control` 手順で `baseline -> candidate`,
  `candidate -> baseline` を実施

search-only `perf stat` 結果:

| order | engine | final nps | cycles / node | instructions / node |
| --- | --- | ---: | ---: | ---: |
| 1 | baseline | `573,724` | `7,250.3` | `15,799.0` |
| 1 | candidate | `560,258` | `7,418.6` | `15,789.3` |
| 2 | candidate | `567,649` | `7,339.1` | `15,782.9` |
| 2 | baseline | `573,539` | `7,320.9` | `15,798.0` |

平均:

| engine | avg nps | avg cycles / node | avg instructions / node |
| --- | ---: | ---: | ---: |
| baseline | `573,631.5` | `7,285.6` | `15,798.5` |
| candidate | `563,953.5` | `7,378.8` | `15,786.1` |

分かった事実:

- candidate は 2-order 平均で baseline 比 `-1.69%`
- `instructions / node` はほぼフラット (`-0.08%`)
- 退行の主因は `cycles / node` 悪化 (`+1.28%`)
- つまりこの種の局所 copy 最適化は、LayerStacks 単体 microbench で勝っても
  search 全体では勝てない

判断:

- この fast path は不採用とし、`crates/rshogi-core/src/nnue/feature_transformer_layer_stacks.rs`
  は差し戻した
- 一方、`bench_nnue_eval` に追加した `layer-stack-refresh-cache` /
  `layer-stack-update-cache` モードは今後の一次ゲートとして保持する
