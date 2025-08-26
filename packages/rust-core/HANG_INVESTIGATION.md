# MoveGen Hang Investigation Guide

## 概要
MoveGenハング問題を「分類 → 最小化 → 局所化」の3段階で調査するツールセットです。

## 準備完了
- ✅ Cargo.tomlに`debug = true`追加済み（関数名付きリリースビルド）
- ✅ MoveGenにトレース機能実装済み
- ✅ 調査スクリプト作成済み

## クイックスタート

### 1. 完全分析（推奨）
```bash
# すべてのフェーズを自動実行
./scripts/analyze_hang_complete.sh
```

### 2. 個別実行

#### Phase 1: 分類（CPU/IO/ロック）
```bash
./scripts/collect_hang_evidence.sh

# 結果の見方:
# - HIGH CPU DETECTED → 計算ループ
# - FUTEX WAIT DETECTED → ロック/ミューテックス
# - IO WAIT DETECTED → I/Oブロッキング
```

#### Phase 2: 最小化
```bash
./scripts/minimize_hang.sh

# 最小USIシーケンスを特定
# 例: "position startpos\ngo depth 1" の2行で再現
```

#### Phase 3: 局所化
```bash
./scripts/localize_hang.sh

# MoveGen内の問題フェーズを特定
# 例: MOVEGEN_DISABLE_CHECKERS_PINS=1 でハング回避
```

## 手動デバッグコマンド

### CPU使用率の確認
```bash
# ハングさせてから別ターミナルで
pidof engine-cli | xargs -I{} ps -L -p {} -o pid,tid,pcpu,stat,wchan,comm
```

### システムコールトレース
```bash
strace -f -ttT -e trace=read,write,futex,ppoll -p $(pidof engine-cli) -o strace.log
```

### スタックダンプ取得
```bash
kill -USR1 $(pidof engine-cli)
# TSVフォーマットでstderrに出力される
```

### トレース付き実行
```bash
SKIP_LEGAL_MOVES=0 MOVEGEN_TRACE=pre,checkers_pins,king,pieces,drops,post \
  ./target/release/engine-cli < test_positions.txt 2>&1 | grep "phase="
```

### フェーズ無効化テスト
```bash
# 例: CHECKERS_PINSフェーズを無効化
SKIP_LEGAL_MOVES=0 MOVEGEN_DISABLE_CHECKERS_PINS=1 \
  timeout 5 ./target/release/engine-cli < test_positions.txt
```

## 環境変数リファレンス

### 基本制御
- `SKIP_LEGAL_MOVES=0/1` - 合法手チェック制御（0でハング再現）
- `USE_ANY_LEGAL=0/1` - 早期リターン最適化版の使用

### トレース制御
- `MOVEGEN_TRACE=phase1,phase2,...` - 指定フェーズでトレース出力
  - 利用可能なフェーズ: pre, checkers_pins, king, pieces, rook, bishop, gold, silver, knight, lance, pawn, drops, post

### フェーズ無効化
- `MOVEGEN_DISABLE_CHECKERS_PINS=1` - チェッカー/ピン計算を無効化
- `MOVEGEN_DISABLE_KING=1` - 王の手生成を無効化
- `MOVEGEN_DISABLE_ROOK=1` - 飛車の手生成を無効化
- `MOVEGEN_DISABLE_BISHOP=1` - 角の手生成を無効化
- `MOVEGEN_DISABLE_GOLD=1` - 金の手生成を無効化
- `MOVEGEN_DISABLE_SILVER=1` - 銀の手生成を無効化
- `MOVEGEN_DISABLE_KNIGHT=1` - 桂馬の手生成を無効化
- `MOVEGEN_DISABLE_LANCE=1` - 香車の手生成を無効化
- `MOVEGEN_DISABLE_PAWN=1` - 歩の手生成を無効化
- `MOVEGEN_DISABLE_DROPS=1` - 持駒打ちを無効化

## 結果の解釈

### CPU Loop の場合
- 無限ループの可能性大
- 特定フェーズ内でカウンタを追加して上限チェック
- ビットボード演算の終了条件確認

### Lock/Mutex Wait の場合
- デッドロックまたは初期化順序問題
- 静的初期化（lazy_static, Once）を確認
- ロック取得順序の一貫性確認

### I/O Blocking の場合
- stderrバッファリング問題
- サブプロセスのstderr読み取り確認
- FORCE_FLUSH_STDERR=1で改善するか確認

## 次のステップ

1. `analyze_hang_complete.sh`の結果から問題の型を特定
2. 特定されたフェーズに詳細ログを追加
3. 根本原因に応じた修正を実施
   - 初期化順序の修正
   - ロック粒度の改善
   - I/O処理の非同期化