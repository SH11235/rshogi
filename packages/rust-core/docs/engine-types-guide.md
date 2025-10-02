# エンジンタイプ選択ガイド

このドキュメントは、`EngineType` の選び方と `SearchProfile`/`SearchParams` の連携をまとめた最新ガイドです。すべての EngineType（Stub を除く）は `ClassicBackend` 上で動作し、評価器とプロファイルを組み合わせて性格付けしています。

---

## 1. エンジンタイプ概要

| EngineType | Evaluator | SearchProfile | 代表用途 | 備考 |
|------------|-----------|---------------|----------|------|
| Stub | StubBackend | ― | 移行期の非常用 | 近い将来削除予定（USI から非表示→撤去の段階を踏む） |
| Material | MaterialEvaluator | `basic_material()` | デバッグ・テスト | 枝刈り最小 / 駒割り評価 |
| Enhanced | MaterialEvaluator | `enhanced_material()` | 省メモリ・学習 | フル枝刈り + 駒割り評価 |
| Nnue | NNUEEvaluatorProxy | `basic_nnue()` | 高速分析 | NNUE 評価 + 基本枝刈り |
| EnhancedNnue | NNUEEvaluatorProxy | `enhanced_nnue()` | 対局・最強設定 | NNUE 評価 + フル枝刈り |

`Engine::set_engine_type()` を呼ぶと、該当する `SearchProfile` が `SearchParams` のランタイム値を既定にリセットします。個別チューニングを残したい場合は、切り替え後に `setoption name SearchParams.*` を再送してください。

---

## 2. SearchProfile と SearchParams の連携

`SearchProfile` は枝刈りトグルと数値パラメータを束ねたテンプレートです。EngineType 切り替え時に `SearchProfile::apply_runtime_defaults()` が呼ばれ、以下のパラメータが既定値へ更新されます。

- LMR/LMP/HP/SBP/ProbCut/IID などの数値 (`SearchParams.LMR_K_x100`, `LMP_D{1,2,3}`, `HP_Threshold`, `SBP_{D1,D2}`, `ProbCut_{D5,D6P}`, `IID_MinDepth`)
- 枝刈りトグル (`EnableNMP`, `EnableIID`, `EnableProbCut`, `EnableStaticBeta`, `Razor`, `QSearchChecks`)

### プロファイルごとの主な差分

| SearchProfileKind | 特徴 | 無効化される枝刈り | Razor / Quiet-Check |
|-------------------|------|----------------------|---------------------|
| `BasicMaterial` | 駒割り評価 + 基本枝刈り | IID / ProbCut / Razor | Razor Off / Quiet On |
| `BasicNnue` | NNUE 評価 + 基本枝刈り | IID / ProbCut / Razor | Razor Off / Quiet On |
| `EnhancedMaterial` | 駒割り評価 + フル枝刈り | ― | Razor On / Quiet On |
| `EnhancedNnue` | NNUE 評価 + フル枝刈り | ― | Razor On / Quiet On |

Runtime で枝刈りを止めたいときは、例として `setoption name SearchParams.EnableNMP value false` を送信します。プロファイルを切り替えると再び既定値に戻る点に注意してください。

---

## 3. 各エンジンタイプの詳細

### EnhancedNnue（推奨）
- NNUE 評価 + フル枝刈りにより最大の棋力。
- 推奨設定例
  ```
  setoption name EngineType value EnhancedNnue
  setoption name USI_Hash value 256
  setoption name Threads value 4
  ```

### Nnue
- NNUE 評価 + 基本枝刈り。
- 浅い探索を高速に回す用途に適しています。

### Enhanced
- 駒割り評価 + フル枝刈り。
- NNUE を使わずに深く読みたい場合や省メモリ環境向け。

### Material
- 駒割り評価 + 基本枝刈り。
- デバッグや学習用途に最適。（低コストで挙動が追いやすい）

### Stub（将来削除予定）
- 移行期の非常手段として残置しているモック検索器。
- ClassicBackend ソーク完了後に USI オプションから非表示→削除の段階を踏む予定。

---

## 4. シナリオ別推奨設定

| シナリオ | 推奨 EngineType | 主なオプション例 |
|----------|-----------------|-------------------|
| 競技対局・長考 | EnhancedNnue | `USI_Hash=256`, `Threads>=4`, 必要なら `EvalFile` 設定 |
| 高速検討・短考 | Nnue | `USI_Hash=128`, `Threads=2` |
| 省メモリ環境 | Enhanced | `USI_Hash=16`, `Threads=1` |
| デバッグ／教育 | Material | `USI_Hash=16`, `Threads=1` |

`SearchParams` を手動でいじった場合は、切り替え前に `SearchParams.Dump` を実装予定（TODO）とするなど、再適用の手順を決めておくと管理が楽になります。

---

## 5. 技術メモ

- **Enhanced 系枝刈り**: Null Move、LMR、Futility、ProbCut、IID などを段階的に適用。`SearchProfile` が既定の有効/無効を決め、`SearchParams` で細かく調整可能。
- **NNUE 評価**: HalfKP 256x2-32-32-1 アーキテクチャ。重みファイルが必要なため `setoption name EvalFile value <path>` を忘れずに。
- **スタック/メモリ**: 深い探索を行う場合は `export RUST_MIN_STACK=8388608` などスタックサイズを確保。TT サイズは `setoption name USI_Hash` で調整します。
- **Threads オプション**: 現状 ClassicBackend は単スレ実装です。`setoption name Threads` を受理した際は `info string` で「現時点では無効」などの通知を出しています（将来 LazySMP 導入予定）。

---

## 6. まとめ

1. EngineType と SearchProfile が 1:1 で対応し、切り替え時に `SearchParams` が既定値へ戻る。
2. いつでも `setoption name SearchParams.*` で個別調整が可能だが、EngineType を変えたら再送が必要。
3. Stub は非常用バックエンド。ClassicBackend が安定したら段階的に撤去する。

迷ったら「EnhancedNnue + 十分な TT（≥256MB） + 必要な Threads（現状 1 thread 固定）」を基本形にし、用途に応じて Material / Enhanced / Nnue を選択してください。
