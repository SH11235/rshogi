# パフォーマンス分析レポート

このドキュメントは、将棋エンジン（Rust実装）のパフォーマンス計測結果と最適化調査の記録です。

## 計測環境

| 項目 | 値 |
|------|-----|
| CPU | AMD Ryzen 9 5950X 16-Core Processor |
| コア数 | 32 |
| OS | Ubuntu (Linux 6.8.0) |
| アーキテクチャ | x86_64 |
| 計測日 | 2025-12-26 |

---

## NPS計測結果

### 最新ベンチマーク（主開発環境: AMD Ryzen 9 5950X）

計測条件: `--threads 1 --tt-mb 256 --limit-type movetime --limit 20000`

#### NNUE評価時

| 局面 | 説明 | Depth | NPS | bestmove |
|:----:|------|:-----:|----:|----------|
| 1 | 序盤（9手目） | 17 | 1,234,096 | 2e2d |
| 2 | 中盤（詰将棋風） | 19 | 571,893 | 8d8f |
| 3 | 終盤（王手飛車） | 17 | 622,347 | 5d6c+ |
| 4 | 終盤（詰み筋） | 20 | 472,782 | G*2h |
| **平均** | - | - | **725,280** | - |

#### Material評価時（NNUE無効、MaterialLevel=9）

| 局面 | 説明 | Depth | NPS | bestmove |
|:----:|------|:-----:|----:|----------|
| 1 | 序盤（9手目） | 17 | 573,806 | 5i6h |
| 2 | 中盤（詰将棋風） | 19 | 434,878 | 8d7d |
| 3 | 終盤（王手飛車） | 17 | 450,774 | G*6b |
| 4 | 終盤（詰み筋） | 18 | 432,624 | G*1c |
| **平均** | - | - | **473,021** | - |

### VNNI効果測定（別端末: Intel Cascade Lake-X）

| 構成 | NNUE平均NPS | 変化 |
|------|----------:|-----:|
| AVX2（VNNI無効） | - | ベースライン |
| AVX512-VNNI | - | **+13%** |

※ VNNI対応CPUでのみ効果あり（Intel Ice Lake以降、AMD Zen 4以降）

### YaneuraOu比較（主開発環境）

#### 通常ビルド（開発時）

| エンジン | NNUE NPS | Material NPS | 備考 |
|---------|--------:|-------------:|------|
| 本エンジン | 725,280 | 473,021 | `cargo build --release` |
| YaneuraOu | 1,118,219 | 1,545,172 | 参考値 |
| **対YaneuraOu比** | **65%** | **31%** | - |

#### PGOビルド（本番用）

| エンジン | NNUE NPS | 対YO比 | 備考 |
|---------|--------:|-------:|------|
| 本エンジン（PGO前） | 681,366 | 61% | ベースライン |
| **本エンジン（PGO後）** | **723,855** | **65%** | **+6.2%向上** |
| YaneuraOu | 1,118,219 | 100% | 参考値 |

※ PGOビルド: `./scripts/build_pgo.sh`

### PGO (Profile-Guided Optimization) 効果

#### NNUE評価時（本番相当）

計測条件: `./target/release/benchmark --nnue-file ...` (3回実行の平均)

| 状態 | Run 1 | Run 2 | Run 3 | 平均NPS |
|------|------:|------:|------:|--------:|
| PGO前 | - | - | - | 681,366 |
| **PGO後** | 722,809 | 725,249 | 723,507 | **723,855** |

| 指標 | 値 |
|------|-----|
| **NPS向上率** | **+6.2%** |
| 絶対値向上 | +42,489 NPS |

#### Material評価時

計測条件: `./target/release/benchmark` (3回実行の平均)

| 状態 | Run 1 | Run 2 | Run 3 | 平均NPS |
|------|------:|------:|------:|--------:|
| PGO前 | 435,567 | 434,590 | 435,712 | 435,290 |
| **PGO後** | 494,473 | 500,417 | 498,039 | **497,643** |

| 指標 | 値 |
|------|-----|
| **NPS向上率** | **+14.3%** |
| 絶対値向上 | +62,353 NPS |

#### PGOの最適化内容

- 分岐予測最適化（頻繁に取られる分岐を優先配置）
- コードレイアウト最適化（ホットパスを連続メモリに配置）
- インライン判断の改善（実行頻度に基づく）

**注**: NNUE評価はMaterial評価より計算負荷が高いため、PGO効果が相対的に小さくなる（+6.2% vs +14.3%）

### LTO・PGO組み合わせ効果（NNUE、参考値）

| 構成 | 平均NPS | 対ベースライン | 備考 |
|------|--------:|---------------:|------|
| Thin LTO（ベースライン） | 681,366 | - | `lto = "thin"` |
| Full LTO | 692,132 | +1.6% | `lto = "fat"` |
| Thin LTO + PGO | 723,855 | +6.2% | - |
| **Full LTO + PGO** | **728,017** | **+6.8%** | **本番用（推奨）** |

- Full LTO単体: +1.6%
- PGO単体: +6.2%
- Full LTO + PGO: +6.8%（PGOに対して+0.6%の追加効果）

**結論**: 本番リリースでは最大性能を優先し、**Full LTO + PGO**（`--profile production`）を使用。`build_pgo.sh` はこの構成でビルドする

---

## 並列探索効率

計測条件: `--threads 1,8 --tt-mb 256 --limit-type movetime --limit 20000`

### Material評価

| スレッド | NPS | スケール | 効率 |
|---------|----:|--------:|-----:|
| 1 | 473,018 | 1.00x | 100.0% |
| 8 | 3,725,090 | 7.87x | **98.4%** |

### NNUE評価

| スレッド | NPS | スケール | 効率 |
|---------|----:|--------:|-----:|
| 1 | 725,296 | 1.00x | 100.0% |
| 8 | 5,563,645 | 7.67x | **95.8%** |

### 並列効率改善の経緯

**PDQSort最適化**（2025-12-26）により、8スレッド時の並列効率が大幅に改善した。

| 評価 | 改善前効率 | 改善後効率 | 変化 |
|------|----------:|----------:|-----:|
| Material | 71.0% | **98.4%** | **+27pt** |
| NNUE | 71.9% | **95.8%** | **+24pt** |

**原因**: MovePicker内の`partial_insertion_sort`（挿入ソート、O(n²)）を、大きい配列に対してはRust標準ライブラリの`sort_unstable_by`（PDQSort、O(n log n)）に切り替え。これにより、8スレッド同時実行時のL3キャッシュ競合が解消された。

詳細: https://github.com/SH11235/shogi/pull/303

---

## ホットスポット一覧

### NNUE有効時（本番相当）

計測コマンド: `./scripts/perf_profile_nnue.sh`

| 順位 | 関数 | CPU% | 状態 | 備考 |
|------|------|------|------|------|
| 1 | `MovePicker::next_move` | 12.49% | 調査完了 | [詳細](#movepicker-調査完了) |
| 2 | `Network::evaluate` | 4.31% | - | NNUE推論メイン |
| 3 | `search_node` | 3.20% | - | 探索メインループ |
| 4 | `refresh_accumulator` | 2.87% | - | NNUE全計算 |
| 5 | `attackers_to_occ` | 2.83% | - | 利き計算 |
| 6 | `do_move_with_prefetch` | 2.16% | - | 指し手実行 |
| 7 | `update_accumulator` | 1.70% | - | Accumulator差分更新 |
| 8 | `check_move_mate` | 1.61% | - | 1手詰め判定 |
| 9 | `__memmove_avx` | 1.59% | - | メモリコピー |
| 10 | `append_active_indices` | 1.37% | - | 特徴量インデックス取得 |
| - | `partial_insertion_sort` | - | 調査完了 | MovePicker内部（PDQSort最適化済み） |

**注**: kernelオーバーヘッド（`__fsnotify_parent` 4.37%, `dput` 3.43%）はNNUEファイル読み込み時のもので、実際の探索時間には影響しない。

#### NNUE関連の内訳

| 関数 | CPU% | 説明 |
|------|------|------|
| `Network::evaluate` | 4.31% | NNUE推論メイン |
| `refresh_accumulator` | 2.87% | Accumulator全計算（差分更新失敗時） |
| `do_move_with_prefetch` | 2.16% | 指し手実行 |
| `update_accumulator` | 1.70% | Accumulator差分更新 |
| `check_move_mate` | 1.61% | 1手詰め判定 |
| `append_active_indices` | 1.37% | 特徴量インデックス取得 |

### Material評価時（NNUE無効、release build）

計測コマンド: `./scripts/perf_profile.sh`

| 順位 | 関数 | CPU% | 備考 |
|------|------|------|------|
| 1 | `eval_lv7_like` | 20.64% | Material評価のメイン関数 |
| 2 | `MovePicker::next_move` | 17.18% | 指し手選択 |
| 3 | `direction_of` | 12.83% | 方向計算 |
| 4 | `attackers_to_occ` | 4.65% | 利き計算 |
| 5 | `search_node` | 4.57% | 探索メインループ |
| 6 | `do_move_with_prefetch` | 2.88% | 指し手実行 |
| 7 | `update_long_effect_from` | 2.52% | 長い利き更新 |
| 8 | `__memmove_avx` | 2.39% | メモリコピー |
| 9 | `check_move_mate` | 2.11% | 1手詰め判定 |
| 10 | `see_ge` | 1.98% | SEE計算 |

**注**: Material評価は1回の評価計算は軽量だが、評価精度が低いため枝刈りの効率が悪く、NPSはNNUEと同等かそれ以下になることが多い。

---

## ハードウェアカウンタ計測 (perf stat)

計測コマンド: `./scripts/perf_all.sh --perf-stat`

### Large Pages + Prefetch最適化の効果（2025-12-23）

TTにLarge Pages（2MB HugePages）を導入し、prefetchタイミングを前倒しした最適化の効果測定。

#### NNUE有効時

| カウンタ | main (最適化前) | 最適化後 | 変化 |
|---------|---------------:|--------:|-----:|
| dTLB-load-misses | 27,511,878 | 11,048,517 | **-60%** |
| cache-misses | 1,003,669,751 | 1,121,953,415 | +12% |
| branch-misses | 2,648,759,117 | 2,568,873,648 | -3% |

#### Material評価時

| カウンタ | main (最適化前) | 最適化後 | 変化 |
|---------|---------------:|--------:|-----:|
| dTLB-load-misses | 14,818,930 | 1,800,779 | **-88%** |
| cache-misses | 172,032,243 | 187,274,719 | +9% |
| branch-misses | 577,482,490 | 513,059,329 | -11% |

#### 考察

**dTLB-load-missesが大幅減少**:
- NNUE: -60%（27.5M → 11.0M）
- Material: -88%（14.8M → 1.8M）
- Large Pages（2MB）により、TLBエントリあたりのカバー範囲が512倍に拡大（4KB→2MB）
- TTアクセス時のTLBミスが劇的に減少

**cache-missesが微増**:
- NNUE: +12%、Material: +9%
- prefetch前倒しによる投機的プリフェッチが増加した可能性
- ただしdTLBミス減少によるレイテンシ改善で相殺される可能性あり

**branch-missesが減少**:
- NNUE: -3%、Material: -11%
- コード変更による間接的な効果

### 最新計測値

#### NNUE有効時

| カウンタ | 値 | 備考 |
|---------|---:|------|
| dTLB-load-misses | 11,048,517 | データTLBミス |
| cache-misses | 1,121,953,415 | キャッシュミス |
| branch-misses | 2,568,873,648 | 分岐予測ミス |

計測時間: 37.85秒（user: 21.50秒、sys: 16.34秒）

**注**: sys時間が大きいのはNNUEファイル読み込み時のI/Oオーバーヘッド。

#### Material評価時

| カウンタ | 値 | 備考 |
|---------|---:|------|
| dTLB-load-misses | 1,800,779 | データTLBミス |
| cache-misses | 187,274,719 | キャッシュミス |
| branch-misses | 513,059,329 | 分岐予測ミス |

計測時間: 20.03秒（user: 19.70秒、sys: 0.33秒）

---

## 調査完了項目

### MovePicker (調査完了)

**調査日**: 2025-12-18
**結論**: **最適化余地なし** - 現在の実装がYaneuraOu/Stockfishと同等で最適解

#### 背景

perfプロファイルで `MovePicker::next_move` が高いオーバーヘッド（6.63%）を示していたため、最適化の可能性を調査。

#### 内訳

| 関数 | CPU% | 役割 |
|------|------|------|
| `partial_insertion_sort` | 2.32% | 指し手のスコア順ソート |
| `score_quiets` | 1.07% | 静かな手のスコア計算 |
| その他（ステージ遷移等） | 3.16% | オーバーヘッド |

#### 検証した最適化候補

| 候補 | 結果 | 詳細 |
|------|------|------|
| A1. 選択ソート (`pick_best`) | 不採用 | 実装テスト済み: **-24% NPS**の大幅悪化。毎回O(n)走査でO(n²)複雑度 |
| A2. SIMD化 | 対象外 | YaneuraOuでコメントのみ、実装なし |
| A3. limit値調整 | 同一 | 現在の実装（`-3560 * depth`）はYaneuraOuと同一 |
| B1. 遅延評価 | 対象外 | YaneuraOuに実装なし |
| B2. History最適化 | 対象外 | YaneuraOuに実装なし |
| C1. ステージスキップ | 同一 | 現在の実装はYaneuraOuと同一 |

#### YaneuraOuとの比較

YaneuraOuの`movepick.cpp`より:
> 現状、全体時間の6.5〜7.5%程度をこの関数で消費している

**YaneuraOu/Stockfishも同様のオーバーヘッドを認識しているが、解決策を持っていない。**

#### 結論

`partial_insertion_sort`のオーバーヘッド（6-7%）は、指し手順序を適切に保つために必要なコストであり、これ以上の最適化余地はない。

---

### Bitboard256 AVX2 SIMD化 (調査完了)

**調査日**: 2025-12-19
**結論**: **AMD Zen 3環境では効果なし** - フィーチャーフラグで将来の検証用に残す

#### 背景

YaneuraOuではBitboard256（角の利き計算用256bit構造体）にAVX2 SIMD命令を使用している。本エンジンでも同様の最適化を検証。

#### 実装内容

| メソッド | AVX2命令 | 用途 |
|---------|---------|------|
| `BitAnd` | `_mm256_and_si256` | 論理AND |
| `BitOr` | `_mm256_or_si256` | 論理OR |
| `BitXor` | `_mm256_xor_si256` | 論理XOR |
| `new()` | `_mm256_broadcastsi128_si256` | 128bit→256bit複製 |
| `from_bitboards()` | `_mm256_inserti128_si256` | 2つの128bitを結合 |
| `byte_reverse()` | `_mm256_shuffle_epi8` | バイト順反転 |
| `merge()` | `_mm256_extracti128_si256` | 256bit→128bit統合 |

#### ベンチマーク結果

```
計測条件: MaterialLevel=9, Threads=1, movetime=20000ms, target-cpu=native
```

| 構成 | 平均NPS | 変化 |
|-----|--------|-----|
| スカラー版（デフォルト） | 446,587 | ベースライン |
| AVX2版（`--features simd_avx2`） | 442,411 | **-0.9%** |

#### アセンブリ分析

スカラー版とAVX2版で生成されるアセンブリを比較:

- **AVX2版**: `vpand`, `vpor`, `vpxor`, `vinserti128` 等のAVX2命令を使用
- **スカラー版**: `movq` を使用した64bit単位の処理（自動ベクトル化なし）

手動SIMD化は確かに異なるコードを生成しているが、パフォーマンス向上には繋がらなかった。

#### 効果がなかった理由の分析

1. **AMD Zen 3のスカラー性能**: 64bit演算が非常に高速で、AVX2の相対的優位性が小さい
2. **bishop_effectの寄与**: 探索全体に占める`attackers_to_occ`（bishop_effect含む）は3.58%のみ
3. **LLVMの最適化**: スカラーコードでもコンパイラが効率的なコードを生成

#### YaneuraOuとの違い

YaneuraOuでは効果があるとされているが、以下の違いが考えられる:

- **CPU環境**: Intel環境ではAVX2の相対効率が高い可能性
- **コンパイラ**: GCC/MSVCとLLVMで最適化特性が異なる
- **計測条件**: マイクロベンチマーク vs 探索全体のNPS

#### 結論

AMD Zen 3環境では効果なし。ただし、以下の理由でフィーチャーフラグ（`simd_avx2`）として残す:

- Intel環境での将来の検証
- マルチスレッド対応時にメモリ帯域幅がボトルネックになった場合の検証

#### フィーチャーフラグについて

**デフォルトでは `simd_avx2` は無効**です。有効にするには明示的に指定が必要です。

```bash
# デフォルト: スカラー版（simd_avx2 無効）
cargo build --release

# AVX2版を有効化
cargo build --release --features simd_avx2

# ベンチマーク実行時
RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release \
  --features simd_avx2 -- --internal --threads 1 ...
```

#### 並列探索実装時の検証方法

マルチスレッド環境ではメモリ帯域幅がボトルネックになる可能性があり、SIMD版の効果が出る可能性がある。以下の手順で検証を推奨:

**1. スレッド数を変えた比較**

```bash
# スカラー版とAVX2版を各スレッド数で比較
for threads in 1 2 4 8 16; do
  echo "=== Threads: $threads (scalar) ==="
  RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release -- \
    --internal --threads $threads --limit-type movetime --limit 20000

  echo "=== Threads: $threads (AVX2) ==="
  RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release \
    --features simd_avx2 -- \
    --internal --threads $threads --limit-type movetime --limit 20000
done
```

**2. 検証ポイント**

| 項目 | 確認内容 |
|------|---------|
| NPS | スレッド数増加時にAVX2版の相対効率が向上するか |
| bestmove | 同一入力で同一結果が得られるか（探索の非決定性による差異は許容） |
| メモリ帯域 | `perf stat -e cache-misses` でキャッシュミス率を確認 |

**3. perfプロファイル（マルチスレッド）**

```bash
# マルチスレッドでのホットスポット確認
./scripts/perf_profile_nnue.sh --threads 4 --movetime 10000

# キャッシュミス統計
sudo perf stat -e cache-references,cache-misses,L1-dcache-load-misses \
  ./target/release/engine-usi <<< "usi
setoption name Threads value 8
go movetime 10000
quit"
```

**4. 期待される結果**

- スレッド数が少ない場合: スカラー版とAVX2版でほぼ同等
- スレッド数が多い場合: メモリ帯域幅がボトルネックになればAVX2版が有利になる可能性

---

### NNUE Accumulator差分更新 (調査完了)

**調査日**: 2025-12-23
**結論**: **最適化余地なし** - YaneuraOuより高度な実装済みで、これ以上の改善は困難

#### 背景

NNUE評価の高速化のため、Accumulator（特徴量ベクトル）の差分更新効率を調査。`refresh_accumulator`（全計算）が3.33%のオーバーヘッドを占めており、差分更新の成功率向上で削減可能かを検証。

#### 診断結果

`--features engine-core/diagnostics` で差分更新成功率を計測:

```
diff_ok=76.0% | refresh=24.0%
direct=66.5% | ancestor=9.4% | prev_nc=24.0%
```

| 指標 | 値 | 説明 |
|------|---:|------|
| diff_ok | 76.0% | 差分更新成功率 |
| direct | 66.5% | 直前局面から差分更新 |
| ancestor | 9.4% | 祖先探索で差分更新 |
| prev_nc | 24.0% | 直前が未計算（祖先探索を試行） |
| refresh | 24.0% | 全計算にフォールバック |

#### 本実装 vs YaneuraOu

| 項目 | YaneuraOu | 本実装 |
|------|:--------:|:------:|
| 直前局面チェック | ✅ | ✅ |
| 祖先探索 | ❌ なし | ✅ 最大8手前 |
| 複数手差分適用 | ❌ | ✅ `forward_update_incremental` |

#### 祖先探索の効果

- `prev_nc`（直前が未計算）のうち39%を祖先探索で救済
- 約187万回/2000万評価のrefreshを回避

#### `prev_nc`が24%発生する原因

探索の特性（null move、LMRなど）で局面をスキップするため。Accumulator更新ロジックではなく**探索アルゴリズム側の問題**であり、Accumulator差分更新の最適化では解決できない。

#### 結論

本実装はYaneuraOuより高度な差分更新機構（祖先探索、複数手差分適用）を持っており、これ以上の改善余地はない。24%のrefreshは探索アルゴリズムの特性に起因する。

---

## 計測方法

### 前提条件

- Linux環境
- `perf`コマンド（`sudo apt install linux-tools-generic`）
- sudo権限

### スクリプト一覧

| スクリプト | 用途 | 推奨用途 |
|-----------|------|----------|
| `perf_profile_nnue.sh` | NNUE有効時のプロファイリング | **本番相当の計測（推奨）** |
| `perf_profile_debug.sh` | debug buildでシンボル詳細解決 | Material評価時、関数名特定 |
| `perf_profile.sh` | 基本的なホットスポット特定 | 簡易計測 |
| `perf_reuse_search.sh` | SearchWorker再利用効果の測定 | 特定調査用 |
| **`build_pgo.sh`** | **PGO最適化ビルド** | **本番デプロイ用（+14% NPS）** |

### 使用例

```bash
cd packages/rust-core

# NNUE有効時（推奨）
./scripts/perf_profile_nnue.sh --movetime 5000

# 結果は自動保存
ls perf_results/
# 20251218_121359_nnue_release.txt

# 対話的な詳細分析
sudo perf report -i perf_nnue.data
```

### ベンチマーク（NPS計測）

```bash
cd packages/rust-core
# --nnue-file オプションはperf.confで指定で省略可能
# --nnue-file オプションを指定したときはperf.conf の設定をオーバライド
RUSTFLAGS="-C target-cpu=native" cargo run -p tools --bin benchmark --release -- \
  --internal --threads 1 --limit-type movetime --limit 20000 \
  --nnue-file ./path/to/nn.bin \
  --output-dir ./benchmark_results
```

### 計測時のビルドプロファイル

- **差分追跡の基準**: `--release` を使用（本ドキュメントのNPS/perfはここを基準に記録）
- **最高最適化の計測**: `build_pgo.sh`（`--profile production` 相当 / Full LTO + PGO）

### PGOビルド（本番デプロイ用）

```bash
cd packages/rust-core

# PGOビルド実行（約3分）- Full LTO + PGOで最大性能
./scripts/build_pgo.sh

# 効果確認付き
./scripts/build_pgo.sh --verify

# プロファイルデータ削除
./scripts/build_pgo.sh --clean
```

PGOビルドの処理フロー:
1. プロファイル収集用ビルド (`--profile production -C profile-generate`)
2. ベンチマーク実行でプロファイル収集
3. `llvm-profdata merge` でマージ
4. PGO適用ビルド (`--profile production -C profile-use`)

**出力先**: `./target/production/` ディレクトリ

**注意**: 開発中の反復作業には通常ビルドを推奨（高速なイテレーション）。PGOビルドはリリース前の最終計測・本番デプロイ時に使用。

---

## 変更履歴

| 日付 | 内容 |
|------|------|
| 2025-12-18 | 初回計測実施、ホットスポット一覧を記録 |
| 2025-12-18 | MovePicker最適化調査完了（最適化余地なし） |
| 2025-12-18 | ドキュメント作成 |
| 2025-12-18 | Material評価時の計測をrelease buildに更新、シンボル解決修正 |
| 2025-12-18 | 計測結果を再計測値で更新（NNUE: MovePicker 6.55%, refresh 6.40%, Material: eval_lv7_like 24.48%） |
| 2025-12-19 | Bitboard256 AVX2 SIMD化調査完了（AMD Zen 3環境では効果なし、フィーチャーフラグで残す） |
| 2025-12-19 | simd_avx2フィーチャーフラグの説明と並列探索時の検証方法を追加 |
| 2025-12-20 | 計測結果更新（NNUE: MovePicker 8.11%, Network::evaluate 5.86%, refresh 5.70%、Material: eval_lv7_like 25.84%, direction_of 16.12%） |
| 2025-12-21 | 計測結果更新（NNUE: MovePicker 8.83%, AffineTransform 5.93%, refresh 2.27%、Material: eval_lv7_like 26.38%, direction_of 15.88%）。refresh_accumulatorが5.70%→2.27%に大幅改善（AccumulatorとFeatureTransformerへのAlignedBox導入によるメモリアラインメント最適化の効果） |
| 2025-12-21 | 計測結果更新（NNUE: MovePicker 9.07%, Network::evaluate 5.93%, refresh 2.38%、Material: eval_lv7_like 25.51%, direction_of 15.85%）。フラットレポート（nnue_flat.txt）を使用した正確な自己時間計測に基づく更新 |
| 2025-12-22 | 計測結果更新（NNUE: MovePicker 8.76%, AffineTransform 4.68%, refresh 2.44%、Material: eval_lv7_like 26.25%, direction_of 16.16%）。NNUE関連の内訳をフラットレポートに基づき修正（AffineTransform::propagateが主要な処理として明確化）。**改善点**: `__memset_avx2`が2.82%→1.91%に約32%減少（MoveBuffer関連のmemset削減最適化の効果） |
| 2025-12-22 | 計測結果更新（NNUE: MovePicker 8.86%, AffineTransform 4.74%, refresh 2.40%、Material: eval_lv7_like 26.22%, direction_of 16.85%）。Material評価時の順位変動: `__memmove_avx`が9位に上昇、`attackers_to_occ`が10位に |
| 2025-12-22 | 計測結果更新（NNUE: MovePicker 9.52%, network::evaluate 3.74%, refresh 2.49%、Material: eval_lv7_like 25.95%, direction_of 16.25%）。**改善点**: AffineTransformのループ逆転最適化により `network::evaluate` が4.74%→3.74%に約21%減少（外側ループを入力チャンク、内側を出力に変更し、入力ブロードキャストと重みアクセスの連続性を改善）。NNUE推論高速化の結果、`MovePicker` が8.86%→9.52%、`check_move_mate` が2.17%でホットスポット6位に浮上するなど、相対比率が変動 |
| 2025-12-22 | **NPS計測結果セクション追加**。NNUE/Material両方の局面別NPS、YaneuraOu比較表を追加。**VNNI dpbusd命令対応**: AVX512-VNNI対応CPUでNNUE積和演算を1命令化（`_mm256_dpbusd_epi32`）。別端末（Intel Cascade Lake-X）での計測で**+13% NPS向上**を確認 |
| 2025-12-22 | 計測結果更新（NNUE: MovePicker 9.36%, network::evaluate 3.73%, refresh 2.45%、Material: eval_lv7_like 25.78%, direction_of 16.39%）。NPS: NNUE平均 681,366（+1.5%）、Material平均 435,547。YaneuraOu比が60%→61%に微増 |
| 2025-12-22 | **PGO (Profile-Guided Optimization) 導入**: `scripts/build_pgo.sh`追加。NNUE NPS **+6.2%向上**（681,366→723,855）、Material NPS **+14.3%向上**（435,290→497,643）。YaneuraOu比がNNUE 61%→65%に改善。PGO効果の詳細計測結果を追加 |
| 2025-12-22 | **本番ビルドプロファイル追加**: `[profile.production]`をCargo.tomlに追加。Full LTO、codegen-units=1、overflow-checks無効化。WASMビルドで-4.2%サイズ削減（865KB→829KB）。CIデプロイがproductionプロファイルを使用するよう更新 |
| 2025-12-22 | **LTO・PGO組み合わせ効果計測**: Full LTO単体+1.6%、Thin LTO+PGO +6.2%、Full LTO+PGO +6.8%。PGO効果が大きく、Full LTOの追加効果は限定的（+0.6%）。通常はThin LTO+PGOを推奨 |
| 2025-12-22 | **build_pgo.sh を Full LTO + PGO に変更**: 本番リリースでは最大性能を優先し、`--profile production`（Full LTO）を使用するよう変更。出力先は `./target/production/` |
| 2025-12-23 | 計測結果更新（NNUE: MovePicker 9.05%, network::evaluate 3.98%, refresh 2.59%、Material: eval_lv7_like 26.34%, direction_of 16.11%）。NPS: NNUE平均 682,777、Material平均 449,439（+3.2%向上）。Material評価時の順位変動: `do_move`が7位に上昇（3.17%）、`attackers_to_occ`が9位、`__memmove_avx`が10位に。**perfスクリプト修正**: `--call-graph dwarf`を`--call-graph fp`に変更（大規模ネスト配列のDWARF解析によるハング回避） |
| 2025-12-23 | 計測結果更新（NNUE: MovePicker 10.46%, network::evaluate 3.75%, refresh 2.37%、Material: eval_lv7_like 26.07%, direction_of 17.23%）。NPS: NNUE平均 668,968、Material平均 451,135。計測誤差の範囲内で大きな変動なし。Material評価時の`direction_of`が16.11%→17.23%に微増 |
| 2025-12-23 | 計測結果更新（NNUE: MovePicker 11.10%, network::evaluate 3.74%, attackers_to_occ 3.05%、Material: eval_lv7_like 27.39%, direction_of 15.59%）。NPS: NNUE平均 665,521、Material平均 454,422。計測誤差の範囲内。Material評価時の順位変動: `direction_of`が17.23%→15.59%に減少、`see_ge`が9位に新登場（1.58%） |
| 2025-12-23 | 計測結果更新（NNUE: MovePicker 12.78%, network::evaluate 4.31%, search_node 3.00%、Material: eval_lv7_like 28.84%, direction_of 17.51%）。**NPS向上**: NNUE平均 690,008（+3.7%、665,521→690,008）、Material平均 466,427（+2.6%）。YaneuraOu比: NNUE 59%→62%、Material 29%→30%に改善。NNUE順位変動: `do_move_with_prefetch`が6位（2.07%）、`update_accumulator`が7位（1.69%）に浮上。`check_move_mate`が2.03%→1.57%に約23%減少。**perf stat セクション新設**: ハードウェアカウンタ計測結果を追加。**Large Pages + Prefetch最適化効果**: mainブランチとの比較でdTLB-load-missesがNNUE -60%、Material -88%と大幅減少（TTにLarge Pages導入の効果）。cache-missesは微増（+9〜12%）だがTLBミス減少で相殺される可能性 |
| 2025-12-23 | **コード品質改善**: `cont_history_ptr()`と`set_cont_history_for_move()`に`debug_assert!`境界チェック追加、`NonNull<PieceToHistory>`のSAFETYドキュメント追加、`unsafe impl Send`のSAFETYコメント詳細化、MovePicker内のContinuationHistoryインデックス4スキップの理由コメント追加、Sentinel初期化テスト追加 |
| 2025-12-23 | 計測結果更新（NNUE: MovePicker 11.46%, update_xray_for_square 4.43%, network::evaluate 3.49%、Material: eval_lv7_like 28.53%, direction_of 16.78%）。**NPS低下**: NNUE平均 544,882（-21%、690,008→544,882）、Material平均 443,925（-5%）。YaneuraOu比: NNUE 62%→49%、Material 30%→29%に低下。**ホットスポット変動**: `update_xray_for_square`がNNUE 2位（4.43%）、Material 3位（8.88%）に浮上。board_effect機能追加（fix-material-board_effectブランチ）によるオーバーヘッドと推測される |
| 2025-12-23 | 計測結果更新（NNUE: MovePicker 12.37%, Network::evaluate 4.10%, search_node 2.98%、Material: eval_lv7_like 31.01%, direction_of 18.08%）。**NPS大幅回復**: NNUE平均 616,051（+13%、544,882→616,051）、Material平均 476,296（+7%、443,925→476,296）。YaneuraOu比: NNUE 49%→55%、Material 29%→31%に回復。**ホットスポット変動**: `update_xray_for_square`がランク外に（board_effect最適化の効果）、代わりに`update_long_effect_from`がNNUE 10位（1.25%）、Material 5位（3.23%）に。Material評価の`eval_lv7_like`が28.53%→31.01%、`direction_of`が16.78%→18.08%に相対上昇（他の処理が高速化した結果） |
| 2025-12-23 | 計測結果更新（NNUE: MovePicker 10.84%, Network::evaluate 4.35%, refresh 3.33%、Material: eval_lv7_like 28.84%, direction_of 19.24%）。**NPS継続回復**: NNUE平均 679,895（+10%、616,051→679,895）、Material平均 467,583（-2%）。YaneuraOu比: NNUE 55%→61%に大幅回復。**ホットスポット変動**: `update_long_effect_from`がNNUEランク外に（board_effect計算の更なる最適化）。NNUE評価時の`MovePicker`が12.37%→10.84%に減少、`refresh_accumulator`が2.56%→3.33%に相対上昇。Material評価時は`eval_lv7_like`が31.01%→28.84%に減少、`direction_of`が18.08%→19.24%に微増 |
| 2025-12-23 | **NNUE Accumulator差分更新調査完了**（最適化余地なし）。YaneuraOuより高度な実装（祖先探索、複数手差分適用）済み。診断結果: diff_ok=76.0%, refresh=24.0%。24%のrefreshは探索アルゴリズムの特性（null move, LMRなど）に起因 |
| 2025-12-26 | **並列探索効率大幅改善**: PDQSort最適化により8T効率がMaterial 71%→**100.1%**、NNUE 72%→**92.6%**に向上。MovePicker内の挿入ソート（O(n²)）を大きい配列でPDQSort（O(n log n)）に切り替え、L3キャッシュ競合を解消。計測結果更新（NNUE: MovePicker 12.49%, Network::evaluate 4.28%, search_node 3.11%、Material: eval_lv7_like 19.64%, MovePicker 17.39%, direction_of 14.10%）。NPS: NNUE平均 726,439（+6.8%）、Material平均 469,158。YaneuraOu比: NNUE 61%→**65%**に改善。**並列探索効率セクション新設**。**ホットスポット変動**: Material評価でMovePicker::next_moveが8.91%→17.39%に増加し2位に浮上（PDQSort導入でソート時間自体は減少したが、eval_lv7_like等の相対比率が下がったため） |
| 2025-12-26 | 計測結果更新（NNUE: MovePicker 12.49%, Network::evaluate 4.31%, search_node 3.20%、Material: eval_lv7_like 20.64%, MovePicker 17.18%, direction_of 12.83%）。NPS: NNUE平均 725,280（-0.2%、誤差範囲）、Material平均 473,021。並列効率: Material 98.4%、NNUE 95.8%（前回100.1%/92.6%からの変動は誤差範囲）。`skip_size`/`skip_phase`設定削除ブランチでの計測 |
