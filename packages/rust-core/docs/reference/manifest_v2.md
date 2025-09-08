# Manifest v2 リファレンス（NNUE 教師データ生成）

本書は `generate_nnue_training_data` が出力する `manifest.json`（v2）の仕様と解釈を定義します。後方互換性を重視し、将来の拡張は Optional フィールドで行います。

## スコープと基本方針
- `manifest_version`: 文字列 `"2"`。
- **run スコープ**: `summary` は「今回の実行（run）での増分」を要約します。
- **累積スコープ**: manifest トップレベルの `attempted/skipped_timeout/errors` は累積（既存＋今回）。
- **part/親の責務分離**: 分割出力時は part と親で意味が異なります（後述）。

## 代表フィールド
- `generated_at`: 生成時刻（UTC ISO8601）。
- `engine`: 教師エンジン名（例: `material`/`enhanced`/`nnue`/`enhanced-nnue`）。
- `teacher_engine {name,version,commit,usi_opts{hash_mb,multipv,threads,teacher_profile,min_depth}}`: プロビナンス。
- `generation_command`: 生成に使用したコマンド（再現性のため）。
- `seed`: 生成時の安定化用シード（引数から決定）。
- `input {path,sha256,bytes}`: 入力の出自情報。
- `nnue_weights_sha256/nnue_weights`: NNUE 重み（使用時）。
- `preset/overrides`: プリセットと、CLI 明示値（time/nodes/hash/multipv/min_depth）の上書き有無。
- `teacher_profile/multipv/budget{mode,time_ms,nodes}/min_depth/hash_mb/threads_per_engine/jobs`: 実行構成。
- `count`: 出力件数（親では全体、part では `count_in_part`）。
- `calibration`: 自動キャリブの結果（存在時）。
- `attempted/skipped_timeout/errors`: 累積統計（トップレベル）。
- `manifest_scope`: `"aggregate" | "part"`（後述）。
- `compression`: `"gz" | "zst" | null`（part/親ともに）。
- `summary`: run 増分の要約（親のみ信頼）。

## manifest_scope と part/親の運用
- `manifest_scope="part"`（part manifest）
  - `count = count_in_part`。`summary=null`。
  - `attempted/skipped_timeout/errors` などの集計値は**参照非推奨**（親を参照）。
- `manifest_scope="aggregate"`（親 manifest）
  - 集約 `summary` を保持。`count` は全パート合計。

## summary（run 増分）
- `elapsed_sec`: 今回 run の経過秒。
- `throughput {attempted_sps,success_sps}`
  - `attempted_sps = attempted_run / elapsed_sec`
  - `success_sps = success_run / elapsed_sec`
- `rates {timeout,top1_exact,both_exact}`（クランプ 0..1）
  - `timeout = skipped_timeout / attempted_run`（skip_overrun のみを対象）
  - `top1_exact = success_run / attempted_run`
  - `both_exact = both_exact_count / lines_ge2`（採用ライン基準: K=3 差替え済みならそれ）
- `counts {attempted,success,skipped_timeout,errors{parse,nonexact_top1,empty_or_missing_pv}}`
  - `attempted/success` は run 増分、`skipped_timeout/errors` は今回分。
- `ambiguous {threshold_cp,require_exact,count,denom,rate,reran?,with_entropy?}`
  - `reran`: K=3 を実行した件数（Option）。
  - `with_entropy`: エントロピーを算出できた件数（Option）。

## JSONL（学習データ）との対応（抜粋）
- `lines_origin = "k2" | "k3"`（採用ラインの由来）。
- K=3 コスト内訳: `time_ms_k2/time_ms_k3/search_time_ms_total`、`nodes_k2/nodes_k3/nodes_total`。
- `softmax_entropy_k3`: K=3 の 3手候補に基づくエントロピー。

## K=3 実行ポリシー（参考）
- 判定条件: `gap2 <= threshold_cp`。`require_exact=false` で非Exactも許容。
- time モードでは K=3 追加予算を `min(rem, limit/4)` とし、**下限 20ms** を適用。

## 後方互換性
- スキーマ拡張は Optional フィールドで行い、既存解析は破壊しません。
- `manifest_version=2` は据え置き。`summary` の run スコープは本ドキュメントで定義。

