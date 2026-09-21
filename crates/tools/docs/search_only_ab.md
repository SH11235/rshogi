# search_only_ab

`search_only_ab` は、baseline / candidate 2 つの USI エンジンを同じ局面・同じ思考時間で
交互に実行し、探索区間だけの cycles/node と instructions/node を比較する A/B ベンチマーク
です。起動・評価関数ロード・`isready` までの時間は計測に含めません。NPS だけでなく
ハードウェアカウンタで比較するので、数 % 未満の差を局面ごとに見分けられます。

## クイックスタート

```bash
cargo build --release -p tools --bin search_only_ab
./target/release/search_only_ab \
  --baseline engines/before.exe --candidate engines/after.exe \
  --positions positions.txt \
  --movetime-ms 10000 --pattern abba --alternate-rounds --rounds 8 \
  --threads 1 --hash-mb 256 --cpu 2 \
  --eval-file /path/to/net.bin --usi-option FV_SCALE=14 \
  --json-out result.json
```

`positions.txt` は 1 行 1 局面で `名前 | position コマンド引数` の形式です
（例: `hirate-like | lnsgkgsnl/1r7/... b - 9`、`startpos` も可）。

## backend

| OS | 計測方法 | 権限 |
|---|---|---|
| Linux | `perf stat --control` で探索区間だけカウンタを有効化 (`--perf-events` で指定) | perf_event_paranoid の許す範囲 |
| Windows | ETW NT Kernel Logger の CSwitch イベントに PMC を添付し、対象スレッドの実行スライスごとに差分を積算 (`--pmc-sources` で指定、既定 `TotalCycles,InstructionRetired`) | 管理者権限が必須 (Hyper-V / VBS 共存可) |

### Windows backend の計測区間と回収

- 計測 window は `position` + `go` 送信直前から `bestmove` 受信直後までの QPC 区間。
  window の境界を跨ぐ実行スライスは重なり比で線形按分する。
- ETW セッションと real-time consumer は **run ごとに作り直す**。エンジン終了後に
  セッションを STOP し、consumer の `ProcessTrace` が自然終了するまで待ってから集計する
  (STOP→drain)。pin した idle コアではエンジンが数秒無中断で走り、`bestmove` 時の最終
  switch-out 1 件だけがその CPU の未満杯バッファに残るため、FLUSH と固定時間待ちでは
  配送されずに計測区間の大半が欠けることがある。
- Thread Start は追跡するが Thread End では所属を外さない (終了スレッドの最終 CSwitch
  が End より後に処理されることがある)。所属は run 終了時に全消去する。

### 破棄 (bail) する条件

次のいずれかを検出した run は計測値を採用せず、エラーで終了する。静かに過小計測した
値を正常値として返さないための設計。

| 条件 | 意味 |
|---|---|
| 最終スライス未到着 (`unclosed_target_slices`) | drain 後も対象スレッドが走ったままの CPU がある = bestmove 時の switch-out が届いていない |
| 連鎖不一致 (`chain_breaks_target`) | 前回 switch-in した TID と今回 switch-out した TID が違う、または timestamp 逆転。対象スレッドが絡む区間でのみ数える |
| PMC 欠落 (`pmc_gaps_target`) | 対象スライスの両端どちらかの CSwitch に PMC が付いていない (基準は TID・時刻だけ保持して次の switch-out で判定) |
| TID 再利用 (`tid_reuse_target`) | run 中に対象所属の TID が別 PID の Thread Start で再利用された。配送順によって旧スレッドと再利用先を区別できないので run ごと拒否 |
| カウンタ巻き戻り (`regressed_switches`) | 対象スライス両端の PMC が単調増加でない。その差分を帰属できないため、他のスライスが正常でも合計は過小になる |
| drain timeout | STOP 後 15 秒以内に `ProcessTrace` が終了しない (CloseTrace 後さらに 5 秒待って detach) |
| ETW イベントロス | セッション統計の `EventsLost` / `RealTimeBuffersLost` が run 中に増えた |

前回の異常終了で NT Kernel Logger が残っていた場合は停止してから開始する (通知は stdout)。

## 主要オプション

| オプション | 説明 |
|---|---|
| `--baseline` / `--candidate` | 比較する USI エンジンの実行ファイル |
| `--positions` | 局面ファイル |
| `--movetime-ms` | 1 探索の思考時間 (ms) |
| `--pattern` | 実行順。`abba` は baseline→candidate→candidate→baseline |
| `--alternate-rounds` | 偶数 round で A/B ラベルを交換する。既定は無効（従来の固定順序を維持） |
| `--rounds` | pattern の繰り返し回数。順序交互では偶数を推奨 |
| `--threads` / `--hash-mb` | エンジンの Threads / USI_Hash |
| `--cpu N` | 論理 CPU N に pin (両 OS)。`--cpus` による shard 並列は Linux のみ |
| `--eval-file` / `--material-level` | EvalFile / MaterialLevel |
| `--usi-option KEY=VALUE` | 両エンジン共通の setoption。`--baseline-usi-option` / `--candidate-usi-option` で片側だけにも渡せる |
| `--perf-events` (Linux) / `--pmc-sources` (Windows) | 計測するカウンタ |
| `--json-out` | `samples` (run ごと)、`blocks` (局面×round の順序と比)、`summary` (variant ごとの合計と差分 %)、`binaries` (両エンジンの SHA-256 とサイズ) を JSON 出力。両 OS でスキーマ互換 (`cli` ブロックのフィールド名だけ異なる) |

## 計測対象バイナリの同定 (`binaries`)

パスだけでは後から「どのビルドの結果か」を証明できないため、計測を始める前に
baseline / candidate の実行ファイルを 1 回ずつ streaming で読み、SHA-256 とバイト数を
記録する。読めない場合は計測に入らずエラーで終了する。

stdout の先頭に次のヘッダを出すので、ログだけでも対象バイナリを特定できる:

```text
[binary] baseline: sha256=<64 桁の 16 進> size_bytes=<バイト数> path=<--baseline の値>
[binary] candidate: sha256=<64 桁の 16 進> size_bytes=<バイト数> path=<--candidate の値>
```

両者の SHA-256 が一致する場合 (A/A 計測や、USI option で経路を切り替える同一バイナリ内
実験) は `[binary] info: ...` を 1 行追加するだけで、エラーにはしない。

JSON レポートには `binaries` ブロックとして保存する。既存フィールド (`cli` / `samples` /
`blocks` / `summary` など) は変更していない。

```json
"binaries": {
  "baseline":  {"path": "engines/before.exe", "sha256": "<64 桁の 16 進>", "size_bytes": 1234567},
  "candidate": {"path": "engines/after.exe",  "sha256": "<64 桁の 16 進>", "size_bytes": 1234567}
}
```

| フィールド | 意味 |
|---|---|
| `path` | `--baseline` / `--candidate` に渡した表記そのまま (`cli.baseline` / `cli.candidate` と同じ) |
| `sha256` | ファイル内容の SHA-256 (小文字 16 進)。`sha256sum` / `Get-FileHash` の結果と照合できる |
| `size_bytes` | ハッシュ対象として読んだバイト数 |

## 結果の読み方

- `summary.cycles_per_node_delta_pct` が pooled の cycles/node 差。局面ごとの差は
  `samples[]` を `position_name` × `variant` で nodes / cycles / instructions を合計してから割る。
- 同一バイナリ同士の A/A を先に 1 本取り、cycles/node の差が ±0.1 % 程度、局面ごとの
  per-sample の幅が 1 % 以内であることを確認してから A/B を読む。幅が数 % に広がる run は
  背景負荷 (Defender の実時間保護、検索インデクサ、他のビルド等) の汚染を疑う。

## round ごとの順序交互と block JSON

固定の `abba` では candidate が常に中央の 2 枠に入り、ブースト減衰などの非線形な
時間ドリフトが A/B 差に混ざる。`--alternate-rounds` を指定すると、各局面について
round 1 は `abba`、round 2 は `baab`、round 3 は `abba`…と交互になる。
ラベル交換なので `ab` は `ba`、`aab` は `bba` になる（文字列の逆順ではない）。
Linux の shard 間でも同じ round は同じ順序を使う。既定は固定順序のままなので、
旧計測との比較にはフラグを付けずに実行する。round が奇数だと開始側の順序が 1 回多くなる。

`cli.alternate_rounds` にモードを記録する。`blocks[]` は実測 samples から集計し、
round → position_index 順に出力する。1 block は **1 局面 × 1 round**。
threads / MultiPV などの条件は run 全体の `cli` を参照する。

| フィールド | 意味 |
|---|---|
| `round` / `position_index` / `position_name` | round と局面を識別。index はコメント・空行を除いた入力順で 1 始まり。同名局面も別 block に保つ。index は `positions` / `samples` にも記録 |
| `order` | `sequence_index` 順に実行した A/B（例: `abba` / `baab`） |
| `baseline_runs` / `candidate_runs` | block 内の各側の実行数 |
| `nodes_ratio` | 各側の 1 探索あたり平均 nodes の candidate / baseline 比 |
| `nps_ratio` | 各側の合計 nodes / 合計 info time による candidate / baseline 比（整数 NPS への丸めなし） |
| `cycles_per_node_ratio` / `instructions_per_node_ratio` | 各側の合計 counter / 合計 nodes による candidate / baseline 比 |

比 1 が差なし、`100 * (ratio - 1)` が差分 %。分母が 0、片側の sample がない、
必要な counter が欠測など、比が定義できない場合は `null`。
既存の `summary` は従来と同じ pooled 集計であり、信頼区間ではない。
block の比を使って信頼区間を外部集計する場合、同じ round の局面を独立な反復として
水増ししない。例えば局面間の log 比を round 内で平均し、その round 平均を標本とする。

測定前に同一バイナリ・同一オプションの A vs A を、順序交互と固定順序の両方で実施する。
条件・round 数・集計法を先に固定し、順序交互の各条件の 95% 区間が 0 を含むか確認する。
0 を含むまでの再実行や、有意な条件だけの除外は行わない。交互化は系統誤差を軽減するが、
有限標本で全区間が必ず 0 を含む保証ではない。受け入れ未達の run も残す。

CPU pin を使い、開始時に加えて計測中も cargo / rustc と背景負荷を外部記録する。
並走ビルドを検出した結果は採用しない。Linux で複数スレッドを複数コアに pin する場合は、
`taskset -c 2-5 search_only_ab ... --threads 4` のように親から affinity を継承させ、
単一 CPU に制限する `--cpu` と局面を並列化する `--cpus` は指定しない。
