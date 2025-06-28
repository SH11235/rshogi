# 将棋通信対戦機能 実装計画書

## 概要
WebRTC を使用したサーバーレス P2P 通信による、将棋の通信対戦機能を実装する。

## 技術スタック
- **通信**: WebRTC (DataChannel)
- **フロントエンド**: React + TypeScript
- **状態管理**: Zustand
- **WebRTC実装**: Rust + WASM (検証済み)

## Phase 1: 基本的な通信対戦機能

### 1.1 WebRTC接続管理 (src/services/webrtc.ts)
```typescript
// WebRTC接続を管理するサービス
class WebRTCConnection {
  private peer: SimpleWebRTCPeer | null = null;
  private isHost: boolean = false;
  private onMessageCallback: ((message: GameMessage) => void) | null = null;

  async createHost(): Promise<string> {
    // ホストとして接続を作成し、オファーを返す
  }

  async joinAsGuest(offer: string): Promise<string> {
    // ゲストとして接続し、アンサーを返す
  }

  async acceptAnswer(answer: string): Promise<void> {
    // ホストがアンサーを受け入れる
  }

  sendMessage(message: GameMessage): void {
    // メッセージを送信
  }

  onMessage(callback: (message: GameMessage) => void): void {
    // メッセージ受信時のコールバックを設定
  }

  disconnect(): void {
    // 接続を切断
  }
}
```

### 1.2 メッセージ型定義 (src/types/online.ts)
```typescript
export interface GameMessage {
  type: 'move' | 'resign' | 'draw_offer' | 'game_start' | 'sync_state';
  data: any;
  timestamp: number;
  playerId: string;
}

export interface MoveMessage {
  type: 'move';
  data: {
    from: SquareKey;
    to: SquareKey;
    promote?: boolean;
    drop?: PieceType;
  };
  timestamp: number;
  playerId: string;
}

export interface GameStartMessage {
  type: 'game_start';
  data: {
    hostPlayer: 'black' | 'white';
    guestPlayer: 'black' | 'white';
  };
  timestamp: number;
  playerId: string;
}

export interface ConnectionStatus {
  isConnected: boolean;
  isHost: boolean;
  peerId: string;
  connectionState: 'new' | 'connecting' | 'connected' | 'disconnected';
}
```

### 1.3 通信対戦用UIコンポーネント

#### OnlineGameDialog.tsx
```typescript
// 通信対戦の開始ダイアログ
// - ホスト/ゲストの選択
// - オファー/アンサーの表示・入力
// - 接続状態の表示
```

#### ConnectionStatus.tsx
```typescript
// 接続状態を表示するコンポーネント
// - 接続状態のインジケーター
// - 切断時の再接続ボタン
// - ピアIDの表示
```

### 1.4 gameStoreの拡張
```typescript
interface GameStore {
  // 既存のフィールドに追加
  gameMode: 'local' | 'online';
  connectionStatus: ConnectionStatus;
  webrtcConnection: WebRTCConnection | null;
  
  // 新しいアクション
  startOnlineGame: (isHost: boolean) => Promise<string>;
  joinOnlineGame: (offer: string) => Promise<string>;
  acceptOnlineAnswer: (answer: string) => Promise<void>;
  
  // 既存のmakeMoveを拡張
  makeMove: (from: SquareKey, to: SquareKey) => boolean;
  // -> オンラインモードの場合は、相手に指し手を送信
}
```

### 1.5 実装順序
1. WebRTC接続管理クラスの実装
2. メッセージ型の定義
3. 通信対戦用UIコンポーネントの作成
4. gameStoreへの統合
5. 指し手の同期機能実装
6. エラーハンドリングと再接続機能

## Phase 2: 高度な機能 (将来実装)

### 2.1 時間制限機能
- 持ち時間の同期
- 秒読みの実装

### 2.2 ゲーム終了処理
- 投了の実装
- 千日手・持将棋の判定同期

### 2.3 観戦機能
- 複数接続のサポート
- 観戦者モードの追加

### 2.4 棋譜機能
- 棋譜の自動保存
- 棋譜の共有機能

## セキュリティ考慮事項
- 受信した指し手の検証（不正な手を防ぐ）
- 接続情報の安全な共有方法の案内
- プレイヤーIDによるなりすまし防止

## テスト計画
1. 単体テスト
   - WebRTC接続管理のテスト
   - メッセージ送受信のテスト
   
2. 統合テスト
   - 実際の対局フローのテスト
   - 切断・再接続のテスト
   
3. E2Eテスト
   - 2つのブラウザ間での対局テスト

## 実装スケジュール
- Phase 1: 2-3週間
  - Week 1: WebRTC接続管理とUI
  - Week 2: gameStore統合と指し手同期
  - Week 3: テストとバグ修正
  
- Phase 2: 将来的に実装（優先度に応じて）