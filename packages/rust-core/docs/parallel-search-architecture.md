# 並列探索アーキテクチャ

本ドキュメントでは、本エンジンの LazySMP 並列探索アーキテクチャについて説明します。

## 概要

本エンジンは **LazySMP (Lazy Shared Memory Parallelization)** 方式の並列探索を採用しています。LazySMP は複数のスレッドが同一局面を独立に探索し、置換表（TT）を介して情報を共有する並列化手法です。

### 主な特徴

- **スレッド独立性**: 各ワーカーは独自のヒューリスティクス（killer moves, history tables）を保持
- **置換表共有**: 全ワーカーが共通の置換表を参照・更新
- **Root擾乱（Jitter）**: ワーカーごとに異なるシードで手順を擾乱し、探索の多様性を確保
- **決定的動作**: 同一条件（session_id, worker_id, root_key）で再現可能

## アーキテクチャ構成

```
┌─────────────────────────────────────────────────────────┐
│                    ParallelSearcher                      │
│  - スレッド数管理                                          │
│  - 探索結果の統合                                          │
└────────────────────┬────────────────────────────────────┘
                     │
        ┌────────────┴────────────┐
        │                         │
┌───────▼──────┐          ┌──────▼────────────────┐
│ Main Worker  │          │   ThreadPool          │
│ (同期実行)    │          │  - Worker管理         │
│              │          │  - Job配送            │
└──────────────┘          └───────┬───────────────┘
                                  │
                     ┌────────────┴────────────┐
                     │                         │
              ┌──────▼──────┐          ┌──────▼──────┐
              │ Helper 1    │   ...    │ Helper N    │
              │ (非同期)     │          │ (非同期)     │
              └─────────────┘          └─────────────┘
```

### コンポーネント詳細

#### 1. ParallelSearcher
- **役割**: 並列探索の統括
- **責務**:
  - スレッド数に応じた実行モード選択（単スレ時は直接実行）
  - Main workerの同期実行
  - Helper workersへのジョブ配送
  - 探索結果の集計と統合

#### 2. ThreadPool
- **役割**: ワーカースレッドのライフサイクル管理
- **実装**: 常駐型スレッドプール（idle loop方式）
- **特徴**:
  - 共有MPSCキュー（`crossbeam::channel`）によるpull型ジョブ配送
  - Graceful shutdown対応（20msタイムアウト）
  - Worker IDの永続性（1..=N、診断の読みやすさ重視）

#### 3. WorkerLocal
- **役割**: ワーカー固有のリソース管理
- **保持データ**:
  - `Heuristics`: killer moves, history tables（セッション内再利用）
  - `SearchStack`: 探索スタック（MAX_PLY+1要素、ジョブ間でリセット）
  - `rng`: 乱数生成器（jitterシード初期化済み）

#### 4. StopController
- **役割**: 探索停止と進捗管理の中央制御
- **機能**:
  - Session管理（`publish_session`, `clear`）
  - Root snapshot発行（`publish_root_line`, `publish_committed_snapshot`）
  - Finalize調停（`try_claim_finalize`）
  - 停止フラグ制御（`request_stop`）

#### 5. ClassicBackend（探索エンジン）
- **役割**: 実際のalpha-beta探索実行
- **エントリーポイント**:
  - `iterative()`: 反復深化探索（main worker用）
  - `think_with_ctx()`: context渡し高速経路（helpers用）

## データフロー

### 探索開始フロー
```
1. Engine → ParallelSearcher::search()
2. StopController::publish_session() でセッション初期化
3. Main worker: ClassicBackend::iterative() 同期実行
4. Helpers: ThreadPool経由で SearchJob配送
   - compute_jitter_seed() でワーカー固有シード生成
   - WorkerLocal::prepare_for_job() でリソースリセット
   - ClassicBackend::think_with_ctx() 実行
```

### 結果統合フロー
```
1. Main worker: 結果を直接取得
2. Helpers: channel経由で結果収集
3. ParallelSearcher::combine_results()
   - nodes/qnodes: 合計（qnodesは共有カウンタのため最大値）
   - depth: 最深到達深度
   - lines: Main worker優先、Helper補完
4. StopController::publish_committed_snapshot() で最終状態発行
5. StopController::clear() でセッション終了
```

## 共有リソースとスレッドローカルリソース

### 共有リソース
| リソース | 同期方式 | 用途 |
|---------|---------|------|
| TranspositionTable | Arc + 内部ロック | 探索結果のキャッシュ |
| StopController | Arc + AtomicBool/Mutex | 停止制御と進捗管理 |
| qnodes_counter | Arc\<AtomicU64\> | qsearch nodeカウント |
| SearchLimits（一部） | Arc | 探索制限パラメータ |

### スレッドローカルリソース
| リソース | 保持場所 | 管理方針 |
|---------|---------|---------|
| Heuristics | WorkerLocal | セッション内再利用、境界でクリア |
| SearchStack | WorkerLocal（helpers）/ TLS（main） | ジョブ開始時リセット |
| RNG | WorkerLocal | jitterシード初期化、不変 |
| Evaluator state | ClassicBackend | 探索中保持 |

## YaneuraOuとの設計比較

| 項目 | YaneuraOu（C++） | 本実装（Rust） |
|-----|-----------------|---------------|
| スレッド管理 | `Threads` クラス | `ThreadPool` + `ParallelSearcher` |
| Idle loop | `Thread::idle_loop()` | `ThreadPool::worker_loop()` |
| ジョブ配送 | `search()` 直接呼び出し | 共有MPSCキュー（pull型） |
| Heuristics | 部分共有（counter moveなど） | 完全分離（将来ブレンド検討） |
| 停止制御 | `Threads.stop` フラグ | `StopController` + AtomicBool |
| 結果集約 | `bestThread` 選択 | Main worker優先 + Helper補完 |

### 設計判断の根拠

1. **Pull型ジョブ配送**: ワーカー数 < ジョブ数でも自然に対応、将来のwork-stealing準備
2. **Heuristics分離**: Rust所有権モデルに適合、TT依存の重複削減を優先
3. **StopController抽象化**: OOB finalize対応、マルチセッション安全性
4. **Main worker優先**: 深さ・品質で優位、Helperは補完的位置付け

## 拡張性

### 現在の設計で可能な拡張
- **Work-stealing**: 共有キューから優先度付きタスク取得
- **YBWC（Young Brothers Wait Concept）**: SplitPoint導入でcut nodeを並列化
- **Heuristics共有**: `HistoryBlender`で部分的情報マージ
- **適応的スレッド数**: 探索状況に応じた動的調整

### アーキテクチャ変更が必要な拡張
- **完全非同期実装**: 現在のmain worker同期実行を非同期化
- **分散探索**: ノード間通信層の追加

## 環境変数による動作カスタマイズ

| 変数名 | 用途 | デフォルト |
|-------|------|-----------|
| `SHOGI_WORKER_STACK_MB` | ワーカースタックサイズ（MB） | OS依存（2-8MB） |
| `SHOGI_THREADPOOL_METRICS` | メトリクス収集有効化 | 0（無効） |
| `SHOGI_CURRMOVE_THROTTLE_MS` | CurrMoveイベント発火間隔（ms） | 100 |
| `SHOGI_INFO_CURRMOVE` | `currmove/currmovenumber` のUSI出力を有効化（`1/true/on`） | 0（抑制） |
| `SHOGI_TEST_FORCE_JITTER` | ジッター機能強制ON/OFF | 1（有効） |

詳細は `docs/parallel-search-implementation.md` を参照してください。
