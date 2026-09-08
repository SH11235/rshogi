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
  --movetime-ms 10000 --pattern abba --rounds 3 \
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
値を正常値として返さないための設計で、`regressed_switches` (カウンタ巻き戻り) だけは
診断値として JSON に残す。

| 条件 | 意味 |
|---|---|
| 最終スライス未到着 (`unclosed_target_slices`) | drain 後も対象スレッドが走ったままの CPU がある = bestmove 時の switch-out が届いていない |
| 連鎖不一致 (`chain_breaks_target`) | 前回 switch-in した TID と今回 switch-out した TID が違う、または timestamp 逆転。対象スレッドが絡む区間でのみ数える |
| PMC 欠落 (`pmc_gaps_target`) | 対象スライスの両端どちらかの CSwitch に PMC が付いていない (基準は TID・時刻だけ保持して次の switch-out で判定) |
| TID 再利用 (`tid_reuse_target`) | run 中に対象所属の TID が別 PID の Thread Start で再利用された。配送順によって旧スレッドと再利用先を区別できないので run ごと拒否 |
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
| `--rounds` | pattern の繰り返し回数 |
| `--threads` / `--hash-mb` | エンジンの Threads / USI_Hash |
| `--cpu N` | 論理 CPU N に pin (両 OS)。`--cpus` による shard 並列は Linux のみ |
| `--eval-file` / `--material-level` | EvalFile / MaterialLevel |
| `--usi-option KEY=VALUE` | 両エンジン共通の setoption。`--baseline-usi-option` / `--candidate-usi-option` で片側だけにも渡せる |
| `--perf-events` (Linux) / `--pmc-sources` (Windows) | 計測するカウンタ |
| `--json-out` | `samples` (run ごと) と `summary` (variant ごとの合計と差分 %) を JSON 出力。両 OS でスキーマ互換 (`cli` ブロックのフィールド名だけ異なる) |

## 結果の読み方

- `summary.cycles_per_node_delta_pct` が pooled の cycles/node 差。局面ごとの差は
  `samples[]` を `position_name` × `variant` で nodes / cycles / instructions を合計してから割る。
- 同一バイナリ同士の A/A を先に 1 本取り、cycles/node の差が ±0.1 % 程度、局面ごとの
  per-sample の幅が 1 % 以内であることを確認してから A/B を読む。幅が数 % に広がる run は
  背景負荷 (Defender の実時間保護、検索インデクサ、他のビルド等) の汚染を疑う。
