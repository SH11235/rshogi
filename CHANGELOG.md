# Changelog

破壊的変更と移行手順をまとめる。詳細は各 PR / runbook を参照。

## Unreleased — 2026-04 SPSA 系破壊的変更

### tournament CLI

- `--engine-usi-option` はデフォルトで共通 `--usi-option` にマージし、同じキーは
  engine 個別指定が上書きするように変更。旧挙動の完全置換が必要な場合は
  `--strict-engine-usi-option` を指定する。
- engine read timeout 時に、EvalFile 未指定・NNUE 読み込み遅延・isready 中の
  panic を疑うヒントと、取得できた engine stderr の直近行を出すようにした。

### spsa CLI

- `--params <path>` を完全削除 (deprecation alias なし)。代替は `--run-dir <dir>`。
  run-dir 配下に固定レイアウトで派生ファイルを配置する (#579)
- `--init-from` の暗黙スキップを禁止。既存 state がある状態で `--init-from`
  を指定すると `--resume` または `--force-init` が必須 (#576)
- `meta.json` format_version を 3 に bump。旧形式の meta は再開不可 (#576)
- 起動時に `=== SPSA Startup Summary ===` を stderr に出力 (init mode と
  active params 上位 5 件を確認できる) (#577)
- `iter 0 snapshot` を `values.csv` に記録するように変更 (#577)
- `rshogi_to_yo_params`: rshogi default 値の混入を 95% 一致閾値で検知し
  warn/error。`--allow-rshogi-defaults` / `--strict-rshogi-defaults` を新設 (#578)
- `<run-dir>/.lock` で同 run-dir の二重起動を排他制御。残留 lock は
  `--force-unlock` で削除 (#580)
- 既存 state.params + フラグなし起動を bail に変更。canonical なしで
  既存 state を起点にしたい場合は `--use-existing-state-as-init` を明示指定
  (silent fresh start は事故の温床だったため) (#580)
- `meta.json` format_version 3 → 4。`current_params_sha256` を追加し、resume
  時に on-disk state.params の hash と meta が一致しなければ bail (write_params
  → save_meta の transactional 復旧検証) (#580)
- SPSA 正常完了時に `<run-dir>/final.params` を atomic に書き出し。
  `tune.py apply` には `state.params` ではなく `final.params` を渡すこと (#580)

### ファイル名 / パスの移行表

| 旧 | 新 (run-dir 直下) |
|---|---|
| `<run>/tuned.params` | `<run>/state.params` |
| `<run>/tuned.params.meta.json` | `<run>/meta.json` |
| `<run>/tuned.params.values.csv` | `<run>/values.csv` |
| `<run>/tuned.params.stats.csv` | `<run>/stats.csv` |
| `<run>/tuned.params.stats_aggregate.csv` | `<run>/stats_aggregate.csv` |

### CLI 移行表

| 旧 | 新 |
|---|---|
| `spsa --params RUN/tuned.params --init-from CANON ...` | `spsa --run-dir RUN --init-from CANON ...` |
| (resume) `spsa --params RUN/tuned.params --resume ...` | `spsa --run-dir RUN --resume ...` |
| (やり直し) `rm -rf RUN && spsa --params ... --init-from ...` | `spsa --run-dir RUN --init-from CANON --force-init ...` |

### 移行チェックリスト

既存運用スクリプトをこのリポジトリ外で持っているなら、以下のパターンを grep:

```bash
rg 'tuned\.params|--params |\.values\.csv|\.stats\.csv|\.stats_aggregate\.csv|\.meta\.json'
```

旧 run dir からの継続は不可 (`tuned.params` は新 run の `--init-from` に渡し
fresh start で seed として再利用する。詳細は `crates/tools/docs/spsa_runbook.md`
§10.7 参照)。

### 関連 PR

- #576 — safety core (state machine, force-init, meta v3, atomic I/O)
- #577 — observability (iter 0, startup summary, stderr 統一)
- #578 — `rshogi_to_yo_params` の default 検知
- #579 — `--params` 廃止 + `--run-dir` 採用 + ドキュメント整理
- #580 — checkpoint safety (lock + state hash + use-existing 明示化 + final.params)
- #581 — runbook §10.7 命名整理 + run-dir integration test (fake USI engine)
