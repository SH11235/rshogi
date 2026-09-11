# USI エンジンの search-only perf 計測

rshogi USI バイナリの **探索区間のみ** の HW カウンタを計測し、cycles/node, cache-miss 率等を A/B 比較する手順。初期化（モデルロード、allocator 初期化、`isready`）のノイズを排除する。

## いつ使うか

- 複数バイナリ（L0 や feature flag が異なる build）の NPS 退行調査
- cache 関連の最適化効果の定量評価（L3 pressure, L1d miss 率）
- profile 変更（例: Threat profile 0 vs cross-side）による cycles/node 差の切り分け
- `go movetime` のランダム性を補正したい A/B 比較

**NG 例（過去にハマったパターン）**:
- `(cat <<EOF ... EOF; sleep N; echo quit) | perf stat -- engine` → 初期化コスト混入 + タイミング依存 + バッファ問題で再現性なし
- `perf stat -- benchmark --internal` → `benchmark` バイナリが計測対象 feature で build されていないとそもそも動かない
- USI pipe に全コマンドを一気に流すと `go` 実行前に `quit` が読まれる場合がある

## ツール

**本命**: `crates/tools/src/bin/search_only_ab.rs`（Linux = perf、Windows = ETW PMC。Windows は「Windows での計測」節を参照）

Linux 実装:

- `perf stat --control fd:20,21 -D -1` で perf を disable 状態で起動
- Engine を perf の子プロセスとして spawn、fd 20/21 を pre_exec で dup2
- USI 制御: `usi` → option 列挙受信 → `setoption` → `isready` → `readyok`
- **計測区間**: `enable` 送信 → `position ... go movetime ...` → `bestmove` → `disable` 送信
- ACK 同期で perf の on/off を厳密に保証 → 初期化コスト完全排除
- baseline vs candidate の 2 バイナリで `abba` 順序（順序バイアス補正）
- taskset -c N で CPU pinning、`--cpus a,b` で shard 並列
- 結果は stdout のサマリ + `--json-out` で JSON レポート

### build

```bash
cargo build --release -p tools --bin search_only_ab
```

`target/release/search_only_ab` に出る。wrapper 自体は計測対象 feature に依存しないので **release build 1 回で全ケースに使える**。計測対象の USI バイナリ側を各 feature で build 分けする。

## Windows での計測（ETW PMC counting）

Windows では perf の代わりに **OS 標準の ETW PMC counting** を使う（NT Kernel Logger
セッションで CSwitch イベントに PMC カウンタを添付し、エンジンスレッドが switch out
されるたびに per-CPU counter 差分をそのスレッドへ帰属させる。xperf の CPU Usage
Precise と同じ帰属規則）。**Hyper-V / VBS 有効のままで動く**（AMD uProf は Hyper-V
非対応なので使わない）。

- **要管理者権限**。非管理者だと `ERROR_ACCESS_DENIED` で明確にエラーになる
- **非昇格セッションから自律実行するには昇格タスク経由**: タスクスケジューラに
  RunLevel Highest のランナータスクを一度登録しておき（Linux 開発機の NOPASSWD
  sudo 相当のローカルセットアップ）、非昇格プロセスからは job ファイルを書いて
  `Start-ScheduledTask` でトリガーする方式が使える（UAC プロンプトなしで完結する）。
  タスク名・パス等のマシン固有情報は repo には書かず各マシンの運用ノートで管理する。
  この方式でランナーを組む場合の設計上の注意:
  - job ファイル（JSON 等）は python 等で**機械生成**する（heredoc 手書きの
    エスケープ破損で JSON が静かに壊れ、計測時間を丸ごと失った事故がある）
  - 結果は log / exit code / error を別ファイルに分け、待機側は exit ファイルだけで
    なく **error ファイルも監視**する（沈黙 ≠ 実行中）。ランナーが過去の result を
    掃除しない設計なら、**トリガー前に前回の残骸を削除**してから起動する
    （残骸を今回の失敗と誤判定しない）
  - JSON レポートが必要なら `--json-out` を job の引数で明示指定する
- CLI は Linux 版と同一互換。差分は以下のみ:
  - `--perf-events` の代わりに `--pmc-sources`（カンマ区切り、default
    `TotalCycles,InstructionRetired`）。source 名は `wpr -pmcsources` で列挙される
    もの（`CacheMisses`, `DcacheMisses`, `BranchMispredictions` 等を追加可能、最大 8 個。
    ただし cache 系は Zen 5 では 0 を返す — 後述の注意を参照）
  - `--cpus`（shard 並列）は未対応（指定するとエラー）。`--cpu N` は
    `SetProcessAffinityMask` で論理 CPU 1 個に pin + `HIGH_PRIORITY_CLASS`
- 計測区間は `position`+`go` 送信直前〜`bestmove` 受信直後の QPC 区間で gating する。
  ETW セッションは run ごとに作り、エンジン終了後に STOP → consumer の `ProcessTrace`
  が自然終了するまで待ってから集計する (pin した idle コアでは最終 switch-out 1 件だけが
  未満杯バッファに残り、FLUSH + 固定待ちでは配送されないことがある)。最終スライス未到着 /
  CSwitch 連鎖の不一致 / PMC 欠落 / 対象 TID の再利用を検出した run はエラーで破棄される
  (詳細は `crates/tools/docs/search_only_ab.md`)
- JSON レポートは Linux 版と `samples` / `summary` がスキーマ互換（`cli` ブロックのみ
  `perf_events` / `pmc_sources` のフィールド名差あり）。source 名は
  `TotalCycles→cycles` / `InstructionRetired→instructions` /
  `BranchInstructions→branches` / `BranchMispredictions→branch_misses` /
  `CacheMisses→cache_misses` / `DcacheMisses→l1_dcache_load_misses` にマップされ、
  未知の source は `perf.extra.<source名>` に入る。既存の jq 集計レシピはそのまま動く。
  ただし `DcacheMisses` は **load+store 合算**で、Linux の `L1-dcache-load-misses`
  （load のみ）とはセマンティクスが異なる。この欄に限らず counter の意味は OS 間で
  完全一致しないため、**cross-OS で数値を直接比較しない**こと（比較は同一 OS 内の
  A/B 差分で行う）

実行例（管理者 PowerShell / Git Bash）:

```powershell
.\target\release\search_only_ab.exe `
  --baseline <BASELINE_EXE> --candidate <CANDIDATE_EXE> `
  --positions <POSITIONS_TXT> --movetime-ms 10000 --pattern abba --rounds 2 `
  --threads 1 --hash-mb 256 --cpu 2 --eval-file <MODEL_PATH> `
  --pmc-sources TotalCycles,InstructionRetired,CacheMisses `
  --json-out <RESULT_JSON>
```

注意:

- 既存の NT Kernel Logger セッション（xperf / wpr のカーネルトレース等）が生きて
  いると **stop してから** 開始する。計測中に xperf 等を並走させないこと
- counter 差分にはエンジンスレッド実行中の割り込み・DPC 時間が含まれる（CPU Usage
  Precise と同じ）。cycles/node の A/B 比較では両側に同様に乗るため通常は無視できる
- ETW は per-CPU バッファ単位で配送され **timestamp 順の保証がない**。Thread Start
  イベントの到着前にその TID の CSwitch が処理されると帰属から漏れる。対象 PID は
  spawn 直後に登録するが、**計測中（`go` 以降）に lazy spawn されるスレッド**は最初の
  数スライスが漏れうる。`isready` までに全スレッドを起動するエンジンなら影響しない
- 実行スライス（CSwitch 間）が計測 window の境界を跨ぐ場合、counter 差分は window
  との**重なり比で線形按分**される（counter がスライス内で一様に増える近似）。pin
  された idle CPU ではエンジンスレッドが数百 ms〜秒オーダーで switch せずスライスが
  極端に粗くなり、全額計上/全額除外だと A/A で cycles/node ±1% 級の量子化誤差が
  実測された（帰属カバレッジ 86〜97% の揺れ）ための措置。`--threads > 1` でも按分で
  緩和されるが、一様増加近似の誤差と割り込み・DPC 混入は残る
- ETW イベントロス（`EventsLost` / `RealTimeBuffersLost` の増分）を run ごとに検査し、
  検出したらその run はエラーで破棄する（静かに壊れた計測値を出さない）。頻発する
  場合はシステム負荷を下げる
- **Zen 5 では cache 系 source（`DcacheMisses` / `CacheMisses`）が常に 0 を返す**
  （legacy event が未マップの疑い）。`TotalCycles` / `InstructionRetired` は正常。
  Zen 5 機では cache 指標に頼らず cycles/node と instructions/node で判定する

## 前提確認

### 1. perf の利用可否

```bash
perf stat -e cycles,instructions,cache-misses,L1-dcache-load-misses -- true
```

`<not supported>` が出たイベントは使えない。**Zen 3 では `LLC-loads` / `LLC-load-misses` が露出していない**ので、デフォルトの `cache-misses` と `L1-dcache-load-misses` で代用する。AMD 固有イベント `ls_any_fills_from_sys.ext_cache_local`, `ls_any_fills_from_sys.mem_io_local` が CCX 外フィル / DRAM フィルの代替指標として使える。

### 2. 並行プロセスの確認

`search_only_ab` は taskset で 1 コアに pin しても **L3 は CCX 内 8 コア共有**なので、他コアで重い計測・学習プロセスが走っていると cache pressure 指標が汚染される。

```bash
pgrep -af 'rshogi|gensfen|tournament|cargo' | grep -v search_only_ab
top -bn1 -o %CPU | head -20
```

特に以下を注視:
- `gensfen` (学習データ生成、30 並列などで走ると L3 を激しく食う)
- `tournament` (自己対局・棋力比較)
- `cargo build` (計測中 CPU 食う)

走っている場合は停止してもらうか、停止できないなら別の CCX（コア 8〜15）に taskset してもらう。

### 3. turbo boost と governor

```bash
cat /sys/devices/system/cpu/cpufreq/boost
cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor
```

turbo boost 有効 + schedutil では NPS が ±10〜20% 揺れる。cycles/node 指標なら周波数揺れが相殺されるので、turbo 有効のままで OK。ただし複数回計測（`--rounds 2` 以上）で平均を取ること。

### 4. 局面ファイル

標準局面ファイル: `/tmp/search_only_sentinel_4pos.txt` (過去計測で使用)

```
hirate-like | lnsgkgsnl/1r7/p1ppp1bpp/1p3pp2/7P1/2P6/PP1PPPP1P/1B3S1R1/LNSGKG1NL b - 9
complex-middle | l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1
tactical | 6n1l/2+S1k4/2lp4p/1np1B2b1/3PP4/1N1S3rP/1P2+pPP+p1/1p1G5/3KG2r1 b GSN2L4Pgs2p 1
movegen-heavy | l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1
```

形式は `name | position 本体` で、本体は `startpos` / `startpos moves ...` / `sfen ...` / 裸の SFEN いずれも可（`normalize_position_command` で補完される）。

消えていたら上記を `cat > /tmp/search_only_sentinel_4pos.txt <<'EOF' ... EOF` で復元する。

## 実行手順

### 基本形（2 バイナリ A/B）

```bash
taskset -c 0 target/release/search_only_ab \
  --baseline  <BASELINE_BINARY> \
  --candidate <CANDIDATE_BINARY> \
  --positions /tmp/search_only_sentinel_4pos.txt \
  --movetime-ms 10000 \
  --pattern abba \
  --rounds 2 \
  --threads 1 \
  --hash-mb 256 \
  --cpu 0 \
  --eval-file <MODEL_PATH> \
  --material-level none \
  --usi-option LS_BUCKET_MODE=progresskpabs \
  --usi-option LS_PROGRESS_BUCKETS=8 \
  --usi-option LS_PROGRESS_COEFF=$SHOGI_DATA/progress/nodchip_progress_e1_f1_cuda.bin \
  --usi-option FV_SCALE=28 \
  --usi-option Threads=1 \
  --json-out <RESULT_JSON>
```

ポイント:
- `--eval-file` は search_only_ab が `EvalFile=` として渡す。バイナリ側がモデル架構と一致していること（v87 binary に v87 モデル等）
- `--usi-option` は baseline/candidate 両方に流れる。片側だけに渡したい場合は `--baseline-usi-option` / `--candidate-usi-option`
- バイナリが `LS_BUCKET_MODE` を知らない場合は `set_option_if_available` で無視される（usi 初期化時の option 一覧で判定）
- `--pattern abba` で baseline→candidate→candidate→baseline の 4 run/position。順序バイアス補正
- `--rounds 2` で pattern を 2 回繰り返す（計 8 run/position）
- `--cpu 0` で taskset 単一コア固定
- `--cpus 2,4` のように複数 CPU を渡すと shard 並列（早く終わるが L3 共有で互いに汚染するので少数バイナリの深い比較には向かない）

### 3 バイナリ以上を比較するとき

search_only_ab は 2 バイナリ A/B のみ。3 つ以上はペア分割して JSON を集計する。例: v87 を baseline 固定で v91/v92/v93/v94 を回す。

```bash
for C in v91 v92 v93 v94; do
  # 対応するバイナリとモデルに差し替えて実行
  ./target/release/search_only_ab \
    --baseline  /tmp/rshogi-1536-progdiff-c5e4e057-prod \
    --candidate /tmp/rshogi-${C}-... \
    ...
    --json-out /tmp/search_only_v87_vs_${C}.json
done
```

**注意**: feature flag や L0 が異なるバイナリを A/B 比較する場合、baseline と candidate で eval モデルが違う。`--eval-file` を共通で渡すと片側が architecture mismatch でロード失敗するので、各ペアごとに baseline 側のモデルを使うか、`--baseline-usi-option EvalFile=...` / `--candidate-usi-option EvalFile=...` で個別指定する。

## 結果の読み方

### stdout サマリ

```
[shard 1][1] round=1 position=hirate-like order=1 variant=baseline cpu=0
[shard 1] depth=20 nodes=3456789 time=10001ms nps=345674 cycles/node=45.2 instructions/node=89.1
...
baseline: runs=8 total_nodes=... average_nps=... cycles_per_node=...
candidate: runs=8 total_nodes=... average_nps=... cycles_per_node=...
nps_delta_pct=+12.3% cycles_per_node_delta_pct=-11.0% instructions_per_node_delta_pct=-5.2%
```

### 主要指標

| 指標 | 意味 | 解釈 |
|---|---|---|
| `cycles/node` | 1 ノード探索あたりの CPU cycles | **主要指標**。小さいほど速い。周波数揺れを相殺 |
| `instructions/node` | 1 ノードあたり命令数 | 計算量の純粋な指標。cache miss の影響を受けない |
| `cache-misses` / `cache-references` | L2/L3 全体の miss 率 | cache pressure の目安 |
| `L1-dcache-load-misses` / nodes | L1d miss/node | L2 以上へ落ちる量。Threat テーブル散在アクセスで増える |
| `nps_delta_pct` | NPS 差 (%) | candidate が baseline より速ければ +、遅ければ − |

### 有意性の目安（ノイズ床）

Windows ETW backend の A/A 実測（Zen 5 / 9950X3D 機、movetime 3s × abba ×
rounds 3、threads=1・単一 CPU pin、境界スライス線形按分実装後）で pooled の
cycles/node 残差は **±0.13%** だった。この構成でのノイズ床として:

- **pooled 差が ±0.2% 未満なら有意と主張しない**（ABBA + rounds ≥ 3 を揃えた上で）
- ±0.2% はあくまで pooled A/A 残差由来のノイズ床であって A/B の最終判定基準では
  ない。pooled 値には position-mix バイアスが乗る（ハマりどころ 8 参照）ので、
  **ノイズ床を越えた場合の最終判定は per-position で行う**
- 上記数値はこの構成（Windows ETW・3s・pin）での実測。movetime を伸ばせば締まり、
  構成が違えばノイズ床も変わるので、疑わしければその構成で A/A を取り直す
- 本文のレシピ例は `--rounds 2`（探索的な比較向け）。このノイズ床を適用して
  有意性を主張する判定では `--rounds 3` 以上に上げる

### cycles/node vs instructions/node の差分

- **cycles/node が下がり、instructions/node は変わらない** → 同じ計算量で cache miss が減った = cache 最適化が効いている
- **cycles/node と instructions/node が両方下がる** → そもそも計算量が減った（例: profile 削減で命令数が減少）
- **cycles/node が変わらず、nps だけ下がる** → 周波数揺れ or 並行プロセスの影響。再計測

### 同一 L0 で profile だけ変えた場合

cycles/node 差 = cache pressure 差 + 計算量差。instructions/node も見れば分解できる:
- Δcycles/node = Δinstructions/node × CPI + Δ(cache miss * miss penalty)

## JSON レポートの集計

`--json-out` で保存した JSON は以下の構造:

```json
{
  "cli": {...},
  "system_info": {...},
  "positions": [...],
  "samples": [
    {"variant": "Baseline", "round": 1, "position_name": "hirate-like",
     "info": {"depth": 20, "nodes": ..., "nps": ...},
     "perf": {"cycles": ..., "instructions": ..., "cache_misses": ..., ...}}
  ],
  "summary": {
    "baseline": {"average_nps": ..., "cycles_per_node": ..., ...},
    "candidate": {...},
    "nps_delta_pct": ...,
    "cycles_per_node_delta_pct": ...
  }
}
```

複数ペアの JSON を横並びで集計するには `jq` で summary を抜き出す:

```bash
for f in /tmp/search_only_v87_vs_*.json; do
  jq -c '{file: input_filename, nps_delta: .summary.nps_delta_pct, cpn_delta: .summary.cycles_per_node_delta_pct, baseline_cpn: .summary.baseline.cycles_per_node, candidate_cpn: .summary.candidate.cycles_per_node}' "$f"
done
```

## 既知のハマりどころ

### 1. EvalFile architecture mismatch

L0 や feature が違うバイナリに同じ eval file を渡すと panic する (`This method is only for LayerStacks architecture.`)。バイナリと対応するモデルを必ずペアで指定する。

### 2. `go nodes N` 未サポート（search_only_ab 側で）

現行 `search_only_ab` は `go movetime ms` を使用。nodes 指定での比較をしたいなら改修が必要だが、`cycles/node` 指標で cycle 基準の比較ができるので不要な場合が多い。

### 3. `LLC-loads` / `LLC-load-misses` が Zen 3 で不可

`perf_events` で指定すると `<not supported>` で落ちる。デフォルトの `cache-misses` と `L1-dcache-load-misses` で済ませる。L3 specific が欲しければ `ls_any_fills_from_sys.ext_cache_local`, `ls_any_fills_from_sys.mem_io_local` を `--perf-events` で追加指定する。

### 4. taskset -c 0 でも L3 は共有

Zen 3 は 8 コアで L3 (32 MB) を共有する CCX 構成。taskset -c 0 は CCX0 内のコア 0 に pin するだけで、CCX0 内の他コア (1-7) で重いタスクが走ると L3 を食う。測定中は `pgrep -af` で並行プロセスを確認する。

### 5. 複数ペアの結果を比較するとき baseline を同じ条件で何度も測ることになる

`v87 baseline × 4 candidate` 方式だと v87 を 4 回計測することになる。計測コスト 2 倍になるが、各ペアで順序バイアスと TTL 汚染を揃えられるメリットがある。1 回だけ計測したいなら全バイナリを shard 並列でバッチ実行する新ツールが必要（未実装）。

### 6. `--threads 1` と `Threads=1` の二重指定

search_only_ab の `--threads` オプションは `setoption name Threads` として送られる。`--usi-option Threads=1` は二重送信になるが害はない（同じ値なら）。

### 7. 必須 USI オプションは rshogi 側が認識しないと silently skip

`set_option_if_available` が `option name ...` 一覧を見て、そのバイナリが対応していない option は送信しない。**FV_SCALE などが送られずに auto 値が使われて混乱する可能性**があるので、`--verbose` で送信ログを確認するか、engine 単体で `usi` を叩いて `option name` 一覧に含まれているか事前確認する。

### 8. pooled aggregate (`average_nps` / `nps_delta_pct`) は position-mix バイアスが乗る

summary の `average_nps` は全 position 横断で `total_nodes / total_time` を pool した値。
fixed `movetime` 下では position ごとにノード数が違う（速い局面ほど多ノード）ため、pool
すると多ノード局面の重みが大きくなり、**per-position の傾向と符号が逆転しうる**。

実例: ある layout refactor の A/B で pooled `nps_delta_pct` は -1.10% だったが、
per-position 平均では +2.25%（instructions/node はフラット）。pooled だけ見ると退行と
誤判定する。

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

