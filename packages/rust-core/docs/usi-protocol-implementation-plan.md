# USIプロトコル実装計画

## 概要

本ドキュメントは、rust-core将棋エンジンにUSI（Universal Shogi Interface）プロトコルを実装するための詳細な計画を定義します。USIプロトコルは将棋GUIプログラムとエンジン間の標準的な通信プロトコルであり、これを実装することで様々なGUIプログラムでエンジンを使用可能になります。

## 1. USIプロトコル要件

### 1.1 必須コマンド

| コマンド | 説明 | 実装優先度 |
|---------|------|-----------|
| **usi** | エンジン名とオプションを返す | 高 |
| **isready** | 初期化完了を確認 | 高 |
| **position** | 盤面と手順を設定 | 高 |
| **go** | 思考開始 | 高 |
| **stop** | 思考停止 | 高 |
| **quit** | 終了 | 高 |
| **setoption** | オプション設定 | 中 |
| **ponderhit** | Ponder的中通知 | 中 |
| **gameover** | 対局終了通知 | 低 |

### 1.2 goコマンドパラメータ

```
go [ponder] [btime <x>] [wtime <x>] [byoyomi <x>] [binc <x>] [winc <x>]
   [movetime <x>] [depth <x>] [nodes <x>] [infinite]
```

### 1.3 出力形式

- **id**: `id name <name>`, `id author <author>`
- **bestmove**: `bestmove <move> [ponder <move>]`
- **info**: 探索情報（depth, nodes, score, pv等）
- **option**: `option name <name> type <type> default <default>`

## 2. 実装設計

### 2.1 モジュール構成

```
crates/engine-cli/
├── src/
│   ├── main.rs          # メインループ
│   ├── usi/
│   │   ├── mod.rs       # USIモジュール
│   │   ├── parser.rs    # コマンドパーサ
│   │   ├── commands.rs  # コマンド定義
│   │   └── output.rs    # 出力フォーマッタ
│   └── engine_adapter.rs # Engine統合

crates/engine-core/src/
└── shogi/
    └── usi.rs           # Move/SFEN変換
```

### 2.2 データ構造

```rust
// USIコマンド定義
#[derive(Debug, Clone)]
pub enum UsiCommand {
    Usi,
    IsReady,
    SetOption { name: String, value: Option<String> },
    Position { 
        startpos: bool,
        sfen: Option<String>, 
        moves: Vec<String> 
    },
    Go(GoParams),
    PonderHit,
    Stop,
    GameOver { result: GameResult },
    Quit,
}

#[derive(Debug, Clone, Default)]
pub struct GoParams {
    pub ponder: bool,
    pub btime: Option<u64>,
    pub wtime: Option<u64>,
    pub byoyomi: Option<u64>,
    pub binc: Option<u64>,
    pub winc: Option<u64>,
    pub movetime: Option<u64>,
    pub depth: Option<u32>,
    pub nodes: Option<u64>,
    pub infinite: bool,
    pub moves_to_go: Option<u32>,
}

#[derive(Debug, Clone)]
pub enum GameResult {
    Win,
    Lose,
    Draw,
}
```

### 2.3 スレッド設計

```
┌─────────────────┐     ┌──────────────────┐
│  Main Thread    │     │  Worker Thread   │
│                 │     │                  │
│  stdin → parse  │     │  Engine::search  │
│       ↓         │     │       ↓          │
│  UsiCommand     │────→│  SearchLimits    │
│       ↓         │     │       ↓          │
│  handle_command │←────│  SearchResult    │
│       ↓         │     │                  │
│  stdout ← format│     │  info output     │
└─────────────────┘     └──────────────────┘
         ↑                        │
         └────── crossbeam ───────┘
              (stop_flag, info)
```

### 2.4 I/Oループ実装

```rust
// メインループ（簡略版）
fn main_loop() -> Result<()> {
    let (tx, rx) = crossbeam_channel::unbounded();
    let engine = Arc::new(Mutex::new(Engine::new()));
    let stop_flag = Arc::new(AtomicBool::new(false));
    
    // stdin読み取りループ
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let command = parse_usi_command(&line?)?;
        
        match command {
            UsiCommand::Go(params) => {
                let engine = Arc::clone(&engine);
                let stop = Arc::clone(&stop_flag);
                let tx = tx.clone();
                
                // Workerスレッドで探索開始
                thread::spawn(move || {
                    search_worker(engine, params, stop, tx);
                });
            }
            UsiCommand::Stop => {
                stop_flag.store(true, Ordering::Release);
            }
            // ... 他のコマンド処理
        }
    }
}
```

## 3. 実装フェーズ

### Phase 1: 基礎実装（2-3日）

#### タスク1.1: USIコマンドパーサ
- [ ] `usi/commands.rs`: コマンド定義
- [ ] `usi/parser.rs`: パーサ実装
- [ ] エラー処理（不正コマンドは`info string`で通知）

#### タスク1.2: Move/SFEN変換
- [ ] `Move::to_usi()` / `from_usi()` 実装
  - 通常の移動: "7g7f"
  - 成り: "2b8h+"
  - 駒打ち: "P*5d"
- [ ] SFEN パーサ実装
  - startpos対応
  - 持ち駒表記（RBGSNLPrbgsnlp）

### Phase 2: スレッド設計とI/Oループ（2日）

#### タスク2.1: メインループ実装
- [ ] stdin読み取りループ
- [ ] コマンドディスパッチャ
- [ ] 基本的な応答（usi, isready）

#### タスク2.2: 検索スレッド管理
- [ ] crossbeam_channel設定
- [ ] Worker spawn/join
- [ ] stop_flag共有

### Phase 3: Engine統合（2日）

#### タスク3.1: SearchLimits変換
- [ ] USI GoParams → engine_core SearchLimits
- [ ] TimeControl設定（Fischer, Byoyomi, FixedTime）
- [ ] TimeManager統合

#### タスク3.2: Info出力実装
- [ ] SearchInfoフォーマッタ
- [ ] 100ms毎のtick出力
- [ ] PV（主要変化）表示

### Phase 4: オプション管理（1日）

#### タスク4.1: OptionRegistry実装
- [ ] 標準オプション定義
  - USI_Hash (default: 16)
  - USI_Ponder (default: true)
  - Threads (default: 1)
- [ ] setoption処理

### Phase 5: テストとエラー処理（1-2日）

#### タスク5.1: ユニットテスト
- [ ] コマンドパーサテスト
- [ ] Move/SFEN変換テスト
- [ ] round-tripテスト

#### タスク5.2: 統合テスト
- [ ] 基本フロー: `usi → isready → position → go → bestmove`
- [ ] エラーケース
- [ ] 並行性テスト

### Phase 6: Ponder実装（1日）

#### タスク6.1: Ponder対応
- [ ] `go ponder`処理
- [ ] `ponderhit`での時間制限切り替え
- [ ] ponder予想手の管理

## 4. エラー処理方針

### 4.1 エラー通知
```
info string Error: Invalid move format: xxxx
info string Warning: Unknown option: yyyy
```

### 4.2 復旧可能性
- 不正コマンド → スキップして継続
- パニック回避 → Result型使用
- リソースリーク防止 → RAII

## 5. テスト戦略

### 5.1 コマンドラインテスト
```bash
# 基本動作確認
echo -e "usi\nisready\nquit\n" | cargo run

# 探索テスト
echo -e "usi\nisready\nposition startpos\ngo movetime 1000\nquit\n" | cargo run

# Ponderテスト
echo -e "position startpos\ngo ponder\nponderhit\n" | cargo run
```

### 5.2 GUIテスト
- ShogiGUIでの動作確認
- 将棋所での動作確認
- floodgateでの対局テスト

## 6. パフォーマンス目標

- コマンド応答: < 1ms
- info出力頻度: 100ms毎
- メモリ使用量: < 100MB（基本状態）
- CPU使用率: 検索時100%、待機時0%

## 7. 今後の拡張

### 7.1 高度な機能
- Multi-PV（複数の候補手表示）
- 評価値グラフ用の詳細info
- 定跡データベース統合

### 7.2 プロトコル拡張
- USI2対応
- 独自オプション追加
- デバッグコマンド

## 8. リスクと対策

| リスク | 対策 |
|-------|------|
| stdin/stdoutのバッファリング | 行バッファリング明示、flush |
| Workerスレッドのハング | タイムアウト監視 |
| メモリリーク | Arc/Mutexの適切な管理 |
| 文字エンコーディング | UTF-8固定 |

## 9. 実装スケジュール

- **Week 1**: Phase 1-2（基礎とスレッド）
- **Week 2**: Phase 3-4（Engine統合とオプション）
- **Week 3**: Phase 5-6（テストとPonder）
- **Week 4**: 統合テストとリリース準備

## 10. 成功基準

1. ShogiGUIで正常に動作する
2. 時間制御（Fischer、秒読み）が正確
3. Ponderが機能する
4. 1000局の自己対局でクラッシュなし
5. info出力でリアルタイム情報提供

---

本計画に従って実装を進めることで、標準的なUSIプロトコルに準拠した将棋エンジンCLIを構築します。