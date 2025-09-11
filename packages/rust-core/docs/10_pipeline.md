# 10_pipeline — 運用改善（残タスクのみ）

本書は「運用改善」トラックの残タスクだけに絞った実行計画です。測定条件・Gate・ログ/データ契約は `docs/00_charter.md` に従います。

## スコープ
- P2/P3 のみを記載（完了済み項目は削除）
- 優先順位は Now → Next → Later の順

## 優先順位（Now / Next / Later）

### Now（まず着手）
1. #11 学習率スケジュール & ログ（P2） — 仕様: `docs/specs/011_lr_schedule.md`
2. #13 最小ガントレット自動化（P2） — 仕様: `docs/specs/013_gauntlet.md`
3. #17 生成のSFEN入力ストリーミング化 + `--expected-multipv auto`（P3→P2へ昇格） — 仕様: `docs/specs/017_generate_streaming.md`

### Next（次に効かせる）
4. #12 サンプル重み運用（P2） — 仕様: `docs/specs/012_weighting.md`

### Later（価値はあるが急がない）
5. #14 Clap移行 / #15 JSONL圧縮直読 / #16 SIMD（オプトイン）

---

## タスク別 DoD と実装ポイント

### #11 学習率スケジュール & ログ（P2）
- 機能:
  - `--lr-schedule {constant|step|cosine}`
  - `--lr-warmup-epochs`, `--lr-decay-epochs/steps`, `--lr-plateau-patience`（可能なら）
- ログ:
  - JSONL に `global_step, epoch, lr, train_loss, val_loss, val_auc, wall_time`
  - `examples_sec, loader_ratio` を合わせて出力
- 備考:
  - Plateauは任意。検証が存在し、`--lr-plateau-patience > 0` のときのみ有効。係数はスケジュールにオーバーレイ（`multiplier *= 0.5`）して次エポックに一律適用。
  - Plateau発火時は人間可読ログに1行通知（structured_v1の`lr`はplateau反映済）。
- DoD:
  - 既存 run（constant）と再現性が一致
  - Cosine/Step で val 曲線に改善が目視可能（ダッシュボード生成に必要十分なログ）
  - ログスキーマは `00_charter.md` の“ログ契約”に準拠
 - 参考: `docs/schemas/structured_v1.schema.json`
 - 実装状況（tools/train_nnue）:
   - CLIに上記フラグを追加し、バッチ更新毎にスケジュール適用
   - 構造化ログ（`--structured-log <PATH|->`）を、ステップ間隔とエポック末に出力
   - 既定は `constant` で、旧挙動と数値一致（決定論テストで担保）

### #13 最小ガントレット自動化（P2）
- 機能:
  - `tools/gauntlet run base.nnue cand.nnue` を 1 コマンド化
  - 条件固定: `0/1+0.1`, `games=100–200`, `threads=1`, `hash_mb=256`, `book=fixed`, `multipv=1`
  - 出力: 勝率、NPS、引分率、PVスプレッド（MultiPV=3 の score 散らばり）
- DoD:
  - Gate: 勝率 +5%pt かつ NPS ±3% 以内で“昇格”
  - 失敗時は重みを昇格させない（自動ロールバック）
  - 固定条件で誰が回しても同結果（`00_charter.md` 準拠）
- コマンド例:
  ```sh
  cargo run -p tools --bin gauntlet -- \
    --base runs/baseline/nn.bin --cand runs/candidate/nn.bin \
    --time "0/1+0.1" --games 200 --threads 1 --hash-mb 256 \
    --book assets/opening/fixed-100.epd --multipv 1 \
    --json runs/gauntlet/out.json --report runs/gauntlet/report.md
  ```

### #17 生成のストリーミング化 + `--expected-multipv auto`（昇格, P2）
- 実装:
  - `generate_nnue_training_data` が SFEN を逐次読み（stdin/pipe/iterator）
  - `analyze_*` は `final.manifest(aggregated.multipv)` → `aggregate pass2` → CLI 指定の順で参照（CLI は常に最優先）
- DoD:
  - 中/大規模入力でピークメモリがほぼ一定（簡易ベンチの数値をレポート化）
  - 既存 manifest と後方互換、CI に小回帰テストを追加
 - メモリ測定: `/proc/self/status` の `VmHWM`（または `time -v`）を使用
 - 参考: `docs/specs/017_generate_streaming.md`

### #12 サンプル重み運用（P2）
- 機能（tools/train_nnue）:
  - `--weighting {exact|gap|phase|mate}`（複数指定可）
  - 係数（既定=1.0）: `--w-exact`, `--w-gap`, `--w-phase-endgame`, `--w-mate-ring`
  - 設定ファイル（YAML/JSON）: `--config <path>`（優先度: CLI > config > 既定）
  - 適用順序: exact → gap → phase → mate（逐次乗算）
- Gate/レポート:
  - 構造化JSONL（structured v1）の `training_config` にスキームと係数（および `preset`）を出力
  - Gauntlet出力も `training_config` を許容（スキーマ参照）
- DoD:
  - 係数変更で val AUC またはガントレット勝率に有意差（±方向含む）が確認可能
  - 既存 run（係数=1.0）と比較して再現性が崩れない（決定論ユニットテストで担保）

---

## スプリント計画（リセット版）

### Sprint α（計測と昇格の基盤）
- #11 学習率スケジュール & 構造化ログ（v1 スキーマ）
- #13 ガントレット自動化（Gate 連携・PVスプレッド計測）
- `00_charter.md` 作成と反映

### Sprint β（運用の詰めとデータ I/O）
- #17 生成ストリーミング化 + `--expected-multipv auto`
- #12 サンプル重み運用（Gate/ダッシュボード反映）

---

## 付記
- すべての集計レポートは `docs/reports/` に保存する
- Clap/圧縮直読/SIMD（#14–#16）は Later とし、需要と工数を見て順次対応

### Fixtures（クイック検証用）
- Gauntlet 用ブック: `docs/reports/fixtures/opening/representative.epd`（代表）, `.../anti.epd`（アンチ）
- Streaming 入力: `docs/reports/fixtures/psv_sample.psv`
- ログ検証: `docs/reports/fixtures/jsonl_sample.jsonl`（`schemas/structured_v1.schema.json`で検証）
