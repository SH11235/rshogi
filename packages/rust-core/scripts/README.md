このディレクトリは運用目的ごとにサブフォルダへ整理されています。

構成
- nnue/
  - NNUE の評価・メトリクス収集（例: `evaluate-nnue.sh`）
- bench/
  - 探索パラメータの比較・A/B 実験（例: `run_ab_*.sh`, `bench_aspiration_diff.sh`）
  - cases/: 単一ケースの再現（HP/LMR/ProbCut 等）
  - suites/: 複数ケースやスイート実行
- smoke/
  - USI/探索のスモークテスト（MultiPV, stop, OOB, near-hard-gate など）
- analysis/
  - USI ログやダイアグ統計の要約・解析スクリプト
- utils/
  - 小物ユーティリティ（USI ワンショットなど）
