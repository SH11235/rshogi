---
name: usi-perf-measure
description: "USI エンジンの探索区間を search_only_ab で A/B 計測する。Linux perf と Windows ETW の条件、結果の読み方、ノイズの確認、性能改善の採否判定に使う。"
---

# USI エンジンの search-only perf 計測

rshogi USI バイナリの **探索区間のみ** の HW カウンタを計測し、cycles/node, cache-miss 率等を
A/B 比較する手順と、その結果で性能改善を採否判定する基準。初期化（モデルロード、allocator
初期化、`isready`）のノイズを排除する。

ツールの仕様 (CLI、OS 別 backend、破棄条件、JSON スキーマ、順序交互) の正典は
`crates/tools/docs/search_only_ab.md`。この skill には判断と落とし穴だけを書く。

## いつ使うか

- 複数バイナリ（L0 や feature flag が異なる build）の NPS 退行調査
- cache 関連の最適化効果の定量評価（L3 pressure, L1d miss 率）
- profile 変更（例: Threat profile 0 vs cross-side）による cycles/node 差の切り分け
- `go movetime` のランダム性を補正したい A/B 比較

**NG 例**:
- `(cat <<EOF ... EOF; sleep N; echo quit) | perf stat -- engine` → 初期化コスト混入 + タイミング依存 + バッファ問題で再現性なし
- `perf stat -- benchmark --internal` → `benchmark` バイナリが計測対象 feature で build されていないとそもそも動かない
- USI pipe に全コマンドを一気に流すと `go` 実行前に `quit` が読まれる場合がある

## 判定 (性能改善の採否)

1. Issue / spec と過去の実験を読み、候補・仮説・改善の判定基準を**計測前に**決める。
   個人の実測・失敗した候補・再現 script はユーザーの private な実験ノートで管理し、
   この repo には置かない。
2. 同一機・同一 build profile・同一モデルと USI 設定で baseline を再現する。
   並行 build / 学習 / 対局による競合を確認し、汚染された測定を採用しない。
   再現性が足りない場合は A/A でばらつきを測る。別機の固定閾値を流用しない。
3. 探索木を変えない最適化なら、単一 thread・固定 depth で nodes / bestmove / score の
   一致を確認する。不一致を性能改善だけで正当化せず、原因を調べて棋力評価
   ([selfplay](../selfplay/SKILL.md)) の要否を判断する。
4. クリーンな binary で A/B を行い、局面別の差と run 間ばらつきを確認する。
   NPS と cycles/node を採否に使い、instructions/node は機序の補助にする。
   CPU 占有率の低下や命令数減少だけで高速化を断定しない。コード配置差の裏取り
   (ハマりどころ 9) を済ませてから採否を決める。
5. 採用 / 不採用 / 未検証を分けて記録する。環境が整わず測れなければ未検証のまま報告する。
   不採用の試行を公開 PR に積み重ねたり、未実測の値で Issue を更新したりしない。

同一条件の binary build は [edition-build](../edition-build/SKILL.md)、NNUE を変更した場合の
refresh / update 検証は [verify-nnue](../verify-nnue/SKILL.md) を使う。

## ツール

`crates/tools/src/bin/search_only_ab.rs`（Linux = `perf stat --control`、Windows = ETW PMC）。
`usi` → `setoption` → `isready` の後、`position` + `go movetime` から `bestmove` までだけ
カウンタを有効にする。baseline / candidate を `abba` 等の順序で交互に回す。

```bash
cargo build --release -p tools --bin search_only_ab
```

wrapper 自体は計測対象 feature に依存しないので release build 1 回で全ケースに使える。
計測対象の USI バイナリ側を各 feature で build 分けする。

### Windows (ETW PMC) で skill として守ること

仕組みと破棄条件は `crates/tools/docs/search_only_ab.md` の「backend」節を読む。
そこに無い運用上の判断だけ挙げる:

- **要管理者権限**。非昇格セッションから自律実行するなら、RunLevel Highest の
  ランナータスクを一度登録しておき、job ファイルを書いて `Start-ScheduledTask` で
  トリガーする。タスク名・パス等のマシン固有情報は repo に書かない。
  - job ファイル（JSON 等）は python 等で**機械生成**する（heredoc 手書きは
    エスケープ破損で JSON が静かに壊れる）
  - 結果は log / exit code / error を別ファイルに分け、待機側は **error ファイルも
    監視**する（沈黙 ≠ 実行中）。ランナーが過去の result を掃除しない設計なら、
    **トリガー前に前回の残骸を削除**する（残骸を今回の失敗と誤判定しない）
- 既存の NT Kernel Logger セッション（xperf / wpr のカーネルトレース等）と並走させない。
- `--cpus`（shard 並列）は Windows では使えない。`--cpu N` で 1 論理 CPU に pin する。
- 計測中（`go` 以降）に lazy spawn されるスレッドは最初の数スライスが帰属から漏れうる。
  `isready` までに全スレッドを起動するエンジンなら影響しない。
- cache 系 source（`DcacheMisses` / `CacheMisses`）は CPU によって常に 0 を返す
  （AMD Zen 5 で確認）。その機体では cycles/node と instructions/node で判定する。
- `DcacheMisses` は load+store 合算で Linux の `L1-dcache-load-misses`（load のみ）と
  意味が違う。counter の意味は OS 間で一致しないので、**cross-OS で数値を直接比較しない**
  （比較は同一 OS 内の A/B 差分で行う）。

## 前提確認

### 1. perf の利用可否 (Linux)

```bash
perf stat -e cycles,instructions,cache-misses,L1-dcache-load-misses -- true
```

`<not supported>` が出たイベントは使えない。CPU によっては `LLC-loads` /
`LLC-load-misses` が露出しないので、`cache-misses` と `L1-dcache-load-misses` で代用する。
AMD では `ls_any_fills_from_sys.ext_cache_local`, `ls_any_fills_from_sys.mem_io_local` が
CCX 外フィル / DRAM フィルの代替指標として使える (`perf list` で存在を確認してから
`--perf-events` に追加する)。

### 2. 並行プロセスの確認

1 コアに pin しても **L3 は同じ CCX / L3 ドメインのコアで共有**なので、他コアで重い計測・
学習プロセスが走っていると cache pressure 指標が汚染される。開始時と計測中に CPU 上位の
プロセスを確認する (確認のみ。止めるかどうかはユーザーに聞く):

```bash
ps -eo pid,pcpu,etime,args --sort=-pcpu | head -20
```

特に `gensfen` (学習データ生成)、`tournament` (自己対局)、`cargo` / `rustc` (ビルド)、
学習プロセスを注視する。止められないなら、別の L3 ドメインのコアに pin する。

### 3. turbo boost と governor

```bash
cat /sys/devices/system/cpu/cpufreq/boost
cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor
```

turbo 有効 + schedutil では NPS が大きく揺れる。cycles/node 指標なら周波数揺れが相殺される
ので turbo 有効のままでよいが、複数 round で平均を取る。

### 4. 局面ファイル

標準局面ファイル: `crates/tools/docs/fixtures/search_only_sentinel_4pos.txt`
（序盤・複雑な中盤・戦術的局面・指し手生成が重い局面の 4 局面）。
形式は `name | position 本体` で、本体は `startpos` / `startpos moves ...` / `sfen ...` /
裸の SFEN いずれも可。`#` 行と空行は無視される。

## 実行手順

### 基本形（2 バイナリ A/B）

```bash
target/release/search_only_ab \
  --baseline  <BASELINE_BINARY> \
  --candidate <CANDIDATE_BINARY> \
  --positions crates/tools/docs/fixtures/search_only_sentinel_4pos.txt \
  --movetime-ms 10000 \
  --pattern abba --alternate-rounds --rounds 4 \
  --threads 1 \
  --hash-mb 256 \
  --cpu 0 \
  --eval-file <MODEL_PATH> \
  --material-level none \
  --usi-option LS_BUCKET_MODE=progresskpabs \
  --usi-option LS_PROGRESS_BUCKETS=<学習時の bucket 数> \
  --usi-option LS_PROGRESS_COEFF=$SHOGI_DATA/progress/<coeff>.bin \
  --usi-option FV_SCALE=<モデルの記録値> \
  --json-out <RESULT_JSON>
```

ポイント:
- `--eval-file` は `EvalFile=` として渡る。バイナリ側の architecture とモデルが一致していること
- `--usi-option` は baseline/candidate 両方に流れる。片側だけに渡したい場合は `--baseline-usi-option` / `--candidate-usi-option`
- `--pattern abba` + `--alternate-rounds` で順序バイアスと時間ドリフトを補正する。round は偶数にする
- `--cpus 2,4` のように複数 CPU を渡すと shard 並列（早く終わるが L3 共有で互いに汚染するので少数バイナリの深い比較には向かない。Linux のみ）
- `--json-out` は結果ディレクトリに保存し、`/tmp` 等の消える場所に置かない

### 3 バイナリ以上を比較するとき

search_only_ab は 2 バイナリ A/B のみ。3 つ以上は baseline 固定のペアに分割して JSON を集計する。
baseline を毎回測り直すことになるが、各ペアで順序バイアスと時間ドリフトを揃えられる。

```bash
for C in <candidate_label_1> <candidate_label_2>; do
  target/release/search_only_ab \
    --baseline  <BASELINE_BINARY> \
    --candidate <candidate の binary> \
    ... \
    --json-out "<RESULT_DIR>/baseline_vs_${C}.json"
done
```

feature flag や L0 が異なるバイナリでは baseline と candidate で eval モデルが違う。
`--eval-file` を共通で渡すと片側が architecture mismatch でロード失敗するので、
`--baseline-usi-option EvalFile=...` / `--candidate-usi-option EvalFile=...` で個別指定する。

## 結果の読み方

### 主要指標

| 指標 | 意味 | 解釈 |
|---|---|---|
| `cycles/node` | 1 ノード探索あたりの CPU cycles | **主要指標**。小さいほど速い。周波数揺れを相殺 |
| `instructions/node` | 1 ノードあたり命令数 | 計算量の純粋な指標。cache miss の影響を受けない |
| `cache-misses` / `cache-references` | L2/L3 全体の miss 率 | cache pressure の目安 |
| `L1-dcache-load-misses` / nodes | L1d miss/node | L2 以上へ落ちる量。Threat テーブル散在アクセスで増える |
| `nps_delta_pct` | NPS 差 (%) | candidate が baseline より速ければ +、遅ければ − |

結果を報告するときは、レポートの `binaries` の SHA-256 で対象バイナリを特定する。

### 有意性の目安（ノイズ床）

同じ binary を baseline / candidate に指定する A/A 計測で、使用する機体・OS・
探索時間・thread 数・pin 条件でのばらつきを確認する。別環境のノイズ床を流用しない。
A/B の差がそのばらつきを超えた場合も、pooled 値だけで判定せず局面別の差を確認する。
pooled 値には position-mix バイアスが乗る（ハマりどころ 8 参照）。

A/A が測るのは計測の再現性だけで、**ビルドごとのコード配置差は含まない**。baseline と
candidate が別ビルドである限り、A/A のノイズ床より大きい差でも配置差の可能性が残る
（ハマりどころ 9 参照）。

### cycles/node vs instructions/node の差分

- **cycles/node が下がり、instructions/node は変わらない** → 同じ計算量で cache miss が減った = cache 最適化が効いている
- **cycles/node と instructions/node が両方下がる** → そもそも計算量が減った（例: profile 削減で命令数が減少）
- **cycles/node が変わらず、nps だけ下がる** → 周波数揺れ or 並行プロセスの影響。再計測

### 同一 L0 で profile だけ変えた場合

cycles/node 差 = cache pressure 差 + 計算量差。instructions/node も見れば分解できる:
- Δcycles/node = Δinstructions/node × CPI + Δ(cache miss * miss penalty)

### JSON の集計

スキーマ (`samples` / `blocks` / `summary` / `binaries`) は `crates/tools/docs/search_only_ab.md`。
複数ペアの summary を横並びにする:

```bash
for f in <RESULT_DIR>/baseline_vs_*.json; do
  jq -c '{file: input_filename, nps_delta: .summary.nps_delta_pct, cpn_delta: .summary.cycles_per_node_delta_pct, baseline_cpn: .summary.baseline.cycles_per_node, candidate_cpn: .summary.candidate.cycles_per_node}' "$f"
done
```

## 既知のハマりどころ

### 1. EvalFile architecture mismatch

L0 や feature が違うバイナリに同じ eval file を渡すと panic する (`This method is only for LayerStacks architecture.`)。バイナリと対応するモデルを必ずペアで指定する。

### 2. `go nodes N` 未サポート（search_only_ab 側で）

`search_only_ab` は `go movetime ms` を使用する。`cycles/node` 指標で cycle 基準の比較ができるので nodes 指定が不要な場合が多い。

### 3. LLC イベントが露出しない CPU がある

`--perf-events` で `<not supported>` のイベントを指定すると落ちる。前提確認 1 で使えるイベントを確かめてから指定する。

### 4. 1 コア pin でも L3 は共有

pin は同じ L3 ドメインの他コアの負荷を防がない。測定中も並行プロセスを確認する。

### 5. `--threads 1` と `Threads=1` の二重指定

search_only_ab の `--threads` オプションは `setoption name Threads` として送られる。`--usi-option Threads=1` は二重送信になるが害はない（同じ値なら）。

### 6. 必須 USI オプションは binary が認識しないと silently skip

search_only_ab は `option name ...` 一覧を見て、そのバイナリが対応していない option は送信しない。**FV_SCALE などが送られずに既定値が使われて混乱する可能性**があるので、`--verbose` で送信ログを確認するか、engine 単体で `usi` を叩いて `option name` 一覧に含まれているか事前確認する。

### 7. 計測中の背景負荷

開始時だけでなく計測中も cargo / rustc と背景負荷を記録する。並走ビルドを検出した結果は採用しない。

### 8. pooled aggregate (`average_nps` / `nps_delta_pct`) は position-mix バイアスが乗る

summary の `average_nps` は全 position 横断で `total_nodes / total_time` を pool した値。
fixed `movetime` 下では position ごとにノード数が違う（速い局面ほど多ノード）ため、pool
すると多ノード局面の重みが大きくなり、**per-position の傾向と符号が逆転しうる**。
pooled `nps_delta_pct` が −1% でも per-position 平均では +2%（instructions/node はフラット）
ということが起こる。pooled だけ見ると退行と誤判定する。

**判定は per-position で行う。** instructions/node は cache contention と position-mix の
両方に影響されにくいので、「生成コードが変わったか（hot path が実質同一か）」の判定は
これを最優先で見る（フラットなら同一）。JSON の `samples` を position×variant で
グルーピングして平均すると確実:

```bash
jq -r '.samples[] | "\(.position_name)\t\(.variant)\t\(.info.nodes)\t\(.perf.instructions)"' report.json \
  | awk -F'\t' '{nd[$1"|"$2]+=$3; ins[$1"|"$2]+=$4}
                END{for(k in nd) printf "%s\t%.1f insn/node\n", k, ins[k]/nd[k]}' \
  | sort
```

### 9. 同じ binary ペアの反復では、ビルドごとのコード配置差を消せない

LTO を含む最適化ビルドは、source の小さな差で関数の配置・整列・inline 判断が変わり、
hot path と無関係な変更でも cycles/node が動く。同一 source を関数整列だけ変えて
ビルドし直して NPS が 0.5% 以上、同じ変更を別の base へ載せてビルドし直して
+1% 超が −1% 超へ反転した例がある。この差は baseline / candidate の binary が同じである限り
何度測っても同じ向きに出るので、**同一ペアの反復で符号が再現しても実効果の証拠にならない**。
cycles/node は周波数揺れを相殺するが、配置差は相殺しない。

instructions/node が横ばいで cycles/node だけが 1% 前後動く結果は、次のどちらかで裏を取ってから採否を決める。

- **同一 binary 内で経路だけを切り替える**: 比較したい経路を実行時に選べる計測専用ビルドを作り
  （USI option、環境変数、exe 名など）、同じ binary を baseline / candidate の両方に指定する。
  コードが同一なので、残る差はデータ配置や経路そのものの効果になる。レポートの `binaries` で
  SHA-256 が一致していることを確認する。計測専用の切替コードは製品の変更に含めない。
- **配置を変えた複数ビルドで符号の一致を見る**: baseline と candidate の両方を、同じ摂動を加えた
  2〜3 通りでビルドして各組を A/B する。例:
  `RUSTFLAGS="-C target-cpu=native -C llvm-args=-align-all-functions=6"`
  （環境変数の `RUSTFLAGS` は `.cargo/config.toml` の rustflags を置き換えるので、`target-cpu` も明示する）。
  `llvm-args` は LLVM へそのまま渡す指定で、toolchain の更新で名前が変わりうる。ビルドが通ることと、
  摂動なしの build と binary の SHA-256 が変わることを確認する。
  摂動ごとに符号が割れるなら、その大きさの差は配置差と区別できていない。

複数の変更を同じ baseline binary と比べた結果は、baseline の配置が不利な場合に全部が同じだけ
良く見える。変更ごとの効果を足し合わせる前に、まとめて適用した build を同じ方法で測り直す。
