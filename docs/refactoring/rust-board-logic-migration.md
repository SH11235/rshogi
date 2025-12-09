# Rust盤面ロジック移行計画

## 📋 概要

TypeScript側で重複実装されている盤面ロジックをRust Core側に統合し、Desktop（Tauri）とWeb（WASM）の両環境で一貫した信頼性の高い実装を提供する。

## 🎯 背景と動機

### 現状の問題点

1. **二重実装によるバグリスク**
   - `packages/app-core/src/game/board.ts`で初期盤面を手動生成
   - 飛車と角の位置が逆になるバグが発生（2025-12-09）
   - 同様のバグが将来も発生する可能性

2. **信頼性の差**
   - Rust側：詰将棋エンジンとして厳密に実装・テスト済み
   - TypeScript側：簡易的な実装、検証不足

3. **メンテナンスコスト**
   - ロジックの変更時に2箇所修正が必要
   - 整合性の維持が困難

4. **SFEN対応の欠如**
   - TypeScript側でSFENのパース/生成ができない
   - `buildPositionString()`は`startpos moves ...`形式のみ

### 目標

✅ **単一の信頼できる真実の源（Rust Core）**を確立
✅ **Desktop（Tauri）とWeb（WASM）で統一インターフェース**を提供
✅ **TypeScriptは表示層に特化**させる
✅ **段階的な移行**で既存機能を壊さない

---

## 🏗️ アーキテクチャ概要

### 現状のアーキテクチャ

```
┌─────────────────────────────────────────────┐
│           UI Layer (React)                  │
│    packages/ui/components/shogi-board.tsx   │
└──────────────────┬──────────────────────────┘
                   │
┌──────────────────▼──────────────────────────┐
│      TypeScript Logic (重複実装)             │
│    packages/app-core/src/game/board.ts      │
│  - createInitialBoard() ← バグの原因         │
│  - applyMove()                              │
│  - parseMove()                              │
└──────────────────┬──────────────────────────┘
                   │
        ┌──────────┴──────────┐
        │                     │
┌───────▼────────┐   ┌────────▼────────┐
│   Tauri IPC    │   │   WASM Binding  │
│ (engine-tauri) │   │ (engine-wasm)   │
└───────┬────────┘   └────────┬────────┘
        │                     │
        └──────────┬──────────┘
                   │
┌──────────────────▼──────────────────────────┐
│         Rust Core (信頼できる実装)            │
│    packages/rust-core/crates/engine-core    │
│  - Position                                 │
│  - SFEN parser/generator                    │
│  - Legal move generator                     │
└─────────────────────────────────────────────┘
```

### 目標アーキテクチャ

```
┌─────────────────────────────────────────────┐
│           UI Layer (React)                  │
│    packages/ui/components/shogi-board.tsx   │
└──────────────────┬──────────────────────────┘
                   │
┌──────────────────▼──────────────────────────┐
│   TypeScript Presentation Layer             │
│    packages/app-core/src/game/              │
│  - PositionService (統一IF)                  │
│  - 表示用データ変換のみ                        │
└──────────────────┬──────────────────────────┘
                   │
        ┌──────────┴──────────┐
        │                     │
┌───────▼────────┐   ┌────────▼────────┐
│   Tauri IPC    │   │   WASM Binding  │
│ (engine-tauri) │   │ (engine-wasm)   │
│ ✨新API追加     │   │ ✨新API追加      │
└───────┬────────┘   └────────┬────────┘
        │                     │
        └──────────┬──────────┘
                   │
┌──────────────────▼──────────────────────────┐
│    Rust Core (単一の真実の源)                 │
│    packages/rust-core/crates/engine-core    │
│  - Position                                 │
│  - SFEN parser/generator                    │
│  - Legal move generator                     │
│  ✨ JSON serialization                       │
└─────────────────────────────────────────────┘
```

---

## 🔄 Desktop vs Web の実装戦略

### 重要原則

> **Desktop（Tauri Backend）とWeb（WASM）は必ず足並みを揃える**
>
> - すべての新機能は**両環境で同時に実装**
> - **統一インターフェース**を通じて利用
> - 環境差異は抽象化層で吸収

### 実装パスの違い

| 項目 | Desktop (Tauri) | Web (WASM) |
|------|-----------------|------------|
| **Backend** | Rust (Native) via Tauri IPC | Rust (WASM) via wasm-bindgen |
| **通信方式** | IPC (invoke/emit) | Direct function call |
| **パッケージ** | `packages/engine-tauri` | `packages/engine-wasm` |
| **エントリポイント** | `apps/desktop/src-tauri/src/lib.rs` | `packages/rust-core/crates/engine-wasm/src/lib.rs` |
| **型変換** | serde_json | serde-wasm-bindgen / JsValue |

### 統一インターフェースの実装方針

```typescript
// packages/app-core/src/game/position-service.ts
type ReplayResult = { applied: string[]; lastPly: number; board: BoardState; error?: string };

export interface PositionService {
    // 環境に依存しない統一API
    getInitialBoard(): Promise<BoardState>;
    parseSfen(sfen: string): Promise<BoardState>;
    boardToSfen(board: BoardState): Promise<string>;
    getLegalMoves(sfen: string, moves?: string[]): Promise<string[]>;
    replayMovesStrict(sfen: string, moves: string[]): Promise<ReplayResult>;
}

// Desktop実装（関数スタイル）
export function createTauriPositionService(): PositionService {
    return {
        async getInitialBoard() {
            return invoke("get_initial_board");
        },
        // ...
    };
}

// Web実装（関数スタイル）
export function createWasmPositionService(): PositionService {
    return {
        async getInitialBoard() {
            return wasm_get_initial_board();
        },
        // ...
    };
}

// ファクトリー関数で環境判定
export function createPositionService(): PositionService {
    if (typeof window !== "undefined" && "__TAURI__" in window) {
        return createTauriPositionService();
    } else {
        return createWasmPositionService();
    }
}
```

---

## 📦 実装計画

### Phase 1: Rust Core の拡張

#### 1.1 JSON型定義の追加

**ファイル**: `packages/rust-core/crates/engine-core/src/types/json.rs` (新規作成)

```rust
use serde::{Deserialize, Serialize};

/// TypeScript側で使用する駒の型定義
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PieceJson {
    /// "sente" | "gote"
    pub owner: String,
    /// "K" | "R" | "B" | "G" | "S" | "N" | "L" | "P"
    #[serde(rename = "type")]
    pub piece_type: String,
    /// 成駒かどうか
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promoted: Option<bool>,
}

/// 盤面の1マスを表す
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CellJson {
    /// "9a" ~ "1i" 形式
    pub square: String,
    /// 駒（存在しない場合はnull）
    pub piece: Option<PieceJson>,
}

/// 持ち駒
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandJson {
    #[serde(rename = "P", skip_serializing_if = "Option::is_none")]
    pub pawn: Option<u32>,
    #[serde(rename = "L", skip_serializing_if = "Option::is_none")]
    pub lance: Option<u32>,
    #[serde(rename = "N", skip_serializing_if = "Option::is_none")]
    pub knight: Option<u32>,
    #[serde(rename = "S", skip_serializing_if = "Option::is_none")]
    pub silver: Option<u32>,
    #[serde(rename = "G", skip_serializing_if = "Option::is_none")]
    pub gold: Option<u32>,
    #[serde(rename = "B", skip_serializing_if = "Option::is_none")]
    pub bishop: Option<u32>,
    #[serde(rename = "R", skip_serializing_if = "Option::is_none")]
    pub rook: Option<u32>,
}

/// 両者の持ち駒
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandsJson {
    pub sente: HandJson,
    pub gote: HandJson,
}

/// 盤面全体の状態
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardStateJson {
    /// 9x9のセル配列
    pub cells: Vec<Vec<CellJson>>,
    /// 持ち駒
    pub hands: HandsJson,
    /// 手番: "sente" | "gote"
    pub turn: String,
}
```

#### 1.2 変換関数の追加

**ファイル**: `packages/rust-core/crates/engine-core/src/position/json_conversion.rs` (新規作成)

```rust
use crate::position::Position;
use crate::types::{Color, Piece, PieceType, Square};
use super::json::*;

impl Position {
    /// 初期盤面をJSON形式で取得
    pub fn initial_board_json() -> BoardStateJson {
        let mut pos = Position::new();
        pos.set_hirate();
        pos.to_board_state_json()
    }

    /// 現在の盤面をJSON形式に変換
    pub fn to_board_state_json(&self) -> BoardStateJson {
        // 実装: Position -> BoardStateJson
        // ...
    }

    /// JSON形式から盤面を復元
    pub fn from_board_state_json(json: &BoardStateJson) -> Result<Self, String> {
        // 実装: BoardStateJson -> Position
        // ...
    }

    /// SFENをパースしてJSON形式で返す
    pub fn parse_sfen_to_json(sfen: &str) -> Result<BoardStateJson, String> {
        let mut pos = Position::new();
        pos.set_sfen(sfen).map_err(|e| e.to_string())?;
        Ok(pos.to_board_state_json())
    }
}
```

**変更ファイル**: `packages/rust-core/crates/engine-core/src/lib.rs`

```rust
pub mod types;
pub mod position;
// 追加
pub use position::json_conversion;
```

#### 1.3 テストの追加

**ファイル**: `packages/rust-core/crates/engine-core/src/position/json_conversion.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_board_json() {
        let board = Position::initial_board_json();
        assert_eq!(board.turn, "sente");
        assert_eq!(board.cells.len(), 9);

        // 先手の飛車が2h（1,7）にあることを確認
        let rook_cell = &board.cells[7][1];
        assert_eq!(rook_cell.square, "2h");
        assert!(rook_cell.piece.is_some());
        let piece = rook_cell.piece.as_ref().unwrap();
        assert_eq!(piece.owner, "sente");
        assert_eq!(piece.piece_type, "R");

        // 先手の角が8h（7,7）にあることを確認
        let bishop_cell = &board.cells[7][7];
        assert_eq!(bishop_cell.square, "8h");
        assert!(bishop_cell.piece.is_some());
        let piece = bishop_cell.piece.as_ref().unwrap();
        assert_eq!(piece.owner, "sente");
        assert_eq!(piece.piece_type, "B");
    }

    #[test]
    fn test_sfen_roundtrip() {
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let json = Position::parse_sfen_to_json(sfen).unwrap();

        let mut pos = Position::from_board_state_json(&json).unwrap();
        assert_eq!(pos.to_sfen(), sfen);
    }
}
```

#### 1.4 棋譜リプレイAPIの正準化（整合性担保）

- `Position::replay_moves_strict(sfen: &str, moves: &[String]) -> ReplayResultJson` を追加し、**最初の不正手で即中断し、適用済み手数と最終局面を返す**挙動を規定する。
- 返却JSON例:

```rust
#[derive(Serialize, Deserialize)]
pub struct ReplayResultJson {
    pub applied: Vec<String>,     // 実際に適用された手
    pub last_ply: usize,          // 適用に成功した最後のply（0-origin）
    pub board: BoardStateJson,    // 最終局面
    pub error: Option<String>,    // 不正手があれば理由を文字列で返す
}
```

- 受け入れ条件: (1) 不正手が含まれる場合はそこで止まり、`applied.len()` が `last_ply + 1` と一致すること (2) 不正手がない場合は全手が適用され、`error` が `None` になること。
- UI側の扱い: `last_ply` は0-originのため、UI表示やログで手数表示する際は +1 する（保存/同期は0-originのまま）。

#### 1.5 命名・シリアライズ規約（Rust/Tauri/WASM/TSで統一）

- Rust構造体フィールド: `snake_case`（例: `last_ply`）。serdeで外部キーに変換する場合は `#[serde(rename = "last_ply")]` を明示し、TS側では `last_ply` で受け取る。
- TypeScriptドメイン型: 受信時はRustのキーそのまま (`last_ply`)、アプリ内ドメインではキャメルケースへ変換し `lastPly` として扱う（例: `ReplayResult.lastPly`）。
- Hand/Hands のキー・駒種別表記は Rust/TS で完全一致させ、serde rename と TS 型定義を両方更新する。

---

### Phase 2: Tauri Backend の拡張

**ファイル**: `apps/desktop/src-tauri/src/lib.rs`

#### 2.1 新しいコマンドの追加

```rust
use engine_core::position::Position;
use engine_core::types::json::BoardStateJson;

#[tauri::command]
fn get_initial_board() -> Result<BoardStateJson, String> {
    Ok(Position::initial_board_json())
}

#[tauri::command]
fn parse_sfen_to_board(sfen: String) -> Result<BoardStateJson, String> {
    Position::parse_sfen_to_json(&sfen)
}

#[tauri::command]
fn board_to_sfen(board: BoardStateJson) -> Result<String, String> {
    let pos = Position::from_board_state_json(&board)?;
    Ok(pos.to_sfen())
}

// 既存の engine_legal_moves は変更なし（707行目）
```

#### 2.2 コマンドハンドラーへの登録

```rust
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(EngineState::default())
        .invoke_handler(tauri::generate_handler![
            engine_init,
            engine_position,
            engine_search,
            engine_stop,
            engine_option,
            engine_legal_moves,
            // 追加
            get_initial_board,
            parse_sfen_to_board,
            board_to_sfen,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

#### 2.3 棋譜リプレイAPIのエクスポート

- 新コマンド `engine_replay_moves_strict(sfen: String, moves: Vec<String>) -> Result<ReplayResultJson, String>` を追加し、Phase1で実装した `replay_moves_strict` をIPC経由で返す。
- `ReplayResultJson` はそのままJSONシリアライズし、UIが `applied` と `last_ply` を基に手数リストを同期できるようにする。
- 受け入れ条件: 不正手を含む棋譜でもIPC返却の `board` / `applied` / `hands` が一致し、UI/エンジンが同一局面を指すことを軽量統合テストまたは手動確認で検証する（重いGUI E2Eは任意）。

---

### Phase 3: WASM Binding の拡張

**ファイル**: `packages/rust-core/crates/engine-wasm/src/lib.rs`

#### 3.1 新しいWASM関数の追加

```rust
use engine_core::position::Position;
use engine_core::types::json::BoardStateJson;

#[wasm_bindgen]
pub fn wasm_get_initial_board() -> Result<JsValue, JsValue> {
    let board = Position::initial_board_json();
    serde_wasm_bindgen::to_value(&board)
        .map_err(|e| JsValue::from_str(&format!("Serialization error: {e}")))
}

#[wasm_bindgen]
pub fn wasm_parse_sfen_to_board(sfen: String) -> Result<JsValue, JsValue> {
    let board = Position::parse_sfen_to_json(&sfen)
        .map_err(|e| JsValue::from_str(&e))?;
    serde_wasm_bindgen::to_value(&board)
        .map_err(|e| JsValue::from_str(&format!("Serialization error: {e}")))
}

#[wasm_bindgen]
pub fn wasm_board_to_sfen(board_json: JsValue) -> Result<String, JsValue> {
    let board: BoardStateJson = serde_wasm_bindgen::from_value(board_json)
        .map_err(|e| JsValue::from_str(&format!("Deserialization error: {e}")))?;
    let pos = Position::from_board_state_json(&board)
        .map_err(|e| JsValue::from_str(&e))?;
    Ok(pos.to_sfen())
}

#[wasm_bindgen]
pub fn wasm_get_legal_moves(sfen: String, moves_json: Option<String>) -> Result<JsValue, JsValue> {
    // 既存の実装を確認して、必要に応じて追加
    // ...
}
```

#### 3.2 棋譜リプレイAPIのエクスポート

- 新関数 `wasm_replay_moves_strict(sfen: String, moves_json: JsValue) -> Result<JsValue, JsValue>` を追加し、Phase1で実装した `replay_moves_strict` をWASM経由で返す。
- `ReplayResultJson` をserde_wasm_bindgenでそのまま返却し、Web側でも `applied` と `last_ply` で手数を同期できるようにする。
- 受け入れ条件: 不正手を含む棋譜でも返却された `board` / `applied` / `hands` が一致し、UIと手数リストが同期することを軽量統合テストまたは手動確認で検証する（重いGUI E2Eは任意）。

---

### Phase 4: TypeScript統一インターフェースの実装

#### 4.1 統一インターフェースの定義

**ファイル**: `packages/app-core/src/game/position-service.ts` (新規作成)

```typescript
import type { BoardState, PositionState } from "./board";

export interface ReplayResult {
    applied: string[];
    lastPly: number;
    board: BoardState;
    error?: string;
}

/**
 * 盤面ロジックサービスの統一インターフェース
 * Desktop（Tauri）とWeb（WASM）で同一のAPIを提供
 */
export interface PositionService {
    /**
     * 初期盤面を取得
     * Rust側のSFEN_HIRATEから生成された正確な初期配置
     */
    getInitialBoard(): Promise<BoardState>;

    /**
     * SFEN文字列をパースして盤面を取得
     */
    parseSfen(sfen: string): Promise<BoardState>;

    /**
     * 盤面をSFEN文字列に変換
     */
    boardToSfen(board: BoardState): Promise<string>;

    /**
     * 指定された盤面での合法手を取得
     */
    getLegalMoves(sfen: string, moves?: string[]): Promise<string[]>;

    /**
     * 棋譜を厳密に適用し、不正手で即中断して結果を返す
     */
    replayMovesStrict(sfen: string, moves: string[]): Promise<ReplayResult>;
}
```

#### 4.2 Desktop（Tauri）実装

**ファイル**: `packages/app-core/src/game/tauri-position-service.ts` (新規作成)

```typescript
import { invoke } from "@tauri-apps/api/core";
import type { BoardState } from "./board";
import type { PositionService, ReplayResult } from "./position-service";

/**
 * Tauri Backend経由での盤面ロジック実装（関数スタイル）
 */
export function createTauriPositionService(): PositionService {
    const convertToBoard = (json: any): BoardState => {
        const board: BoardState = {} as any;
        for (const row of json.cells) {
            for (const cell of row) {
                board[cell.square as any] = cell.piece;
            }
        }
        return board;
    };

    const convertFromBoard = (board: BoardState): any => {
        // BoardState -> JSON変換
        // 実装...
    };

    return {
        async getInitialBoard(): Promise<BoardState> {
            const result = await invoke<{
                cells: Array<Array<{ square: string; piece: any | null }>>;
                hands: { sente: any; gote: any };
                turn: "sente" | "gote";
            }>("get_initial_board");

            return convertToBoard(result);
        },

        async parseSfen(sfen: string): Promise<BoardState> {
            const result = await invoke<any>("parse_sfen_to_board", { sfen });
            return convertToBoard(result);
        },

        async boardToSfen(board: BoardState): Promise<string> {
            const boardJson = convertFromBoard(board);
            return invoke<string>("board_to_sfen", { board: boardJson });
        },

        async getLegalMoves(sfen: string, moves?: string[]): Promise<string[]> {
            return invoke<string[]>("engine_legal_moves", { sfen, moves });
        },

        async replayMovesStrict(sfen: string, moves: string[]): Promise<ReplayResult> {
            const result = await invoke<{
                applied: string[];
                last_ply: number;
                board: any;
                error?: string;
            }>("engine_replay_moves_strict", { sfen, moves });

            return {
                applied: result.applied,
                lastPly: result.last_ply,
                board: convertToBoard(result.board),
                error: result.error,
            };
        },
    };
}
```

#### 4.3 Web（WASM）実装

**ファイル**: `packages/app-core/src/game/wasm-position-service.ts` (新規作成)

```typescript
import type { BoardState } from "./board";
import type { PositionService, ReplayResult } from "./position-service";

// WASM関数のインポート（実際のパスは環境による）
declare function wasm_get_initial_board(): any;
declare function wasm_parse_sfen_to_board(sfen: string): any;
declare function wasm_board_to_sfen(board: any): string;
declare function wasm_get_legal_moves(sfen: string, moves: string[] | null): string[];
declare function wasm_replay_moves_strict(sfen: string, moves_json: any): any;

/**
 * WASM経由での盤面ロジック実装（関数スタイル）
 */
export function createWasmPositionService(): PositionService {
    const convertToBoard = (json: any): BoardState => {
        const board: BoardState = {} as any;
        for (const row of json.cells) {
            for (const cell of row) {
                board[cell.square as any] = cell.piece;
            }
        }
        return board;
    };

    const convertFromBoard = (board: BoardState): any => {
        // Tauri実装と同じロジック
        // 実装...
    };

    return {
        async getInitialBoard(): Promise<BoardState> {
            const result = wasm_get_initial_board();
            return convertToBoard(result);
        },

        async parseSfen(sfen: string): Promise<BoardState> {
            const result = wasm_parse_sfen_to_board(sfen);
            return convertToBoard(result);
        },

        async boardToSfen(board: BoardState): Promise<string> {
            const boardJson = convertFromBoard(board);
            return wasm_board_to_sfen(boardJson);
        },

        async getLegalMoves(sfen: string, moves?: string[]): Promise<string[]> {
            return wasm_get_legal_moves(sfen, moves ?? null);
        },

        async replayMovesStrict(sfen: string, moves: string[]): Promise<ReplayResult> {
            const result = wasm_replay_moves_strict(sfen, moves);
            return {
                applied: result.applied,
                lastPly: result.last_ply,
                board: convertToBoard(result.board),
                error: result.error,
            };
        },
    };
}
```

#### 4.4 ファクトリー関数

**ファイル**: `packages/app-core/src/game/index.ts`

```typescript
import type { PositionService } from "./position-service";
import { createTauriPositionService } from "./tauri-position-service";
import { createWasmPositionService } from "./wasm-position-service";

let cachedService: PositionService | null = null;

/**
 * 環境に応じた適切なPositionServiceを返す
 */
export function getPositionService(): PositionService {
    if (cachedService) {
        return cachedService;
    }

    // Tauri環境かどうかを判定
    const isTauri =
        typeof window !== "undefined" &&
        "__TAURI__" in window;

    cachedService = isTauri
        ? createTauriPositionService()
        : createWasmPositionService();

    return cachedService;
}

// 既存のエクスポートも維持
export * from "./board";
export * from "./position-service";
```

---

### Phase 5: 既存コードの移行

#### 5.1 `createInitialBoard`の置き換え

**ファイル**: `packages/app-core/src/game/board.ts`

```typescript
import { getPositionService } from "./index";

/**
 * @deprecated Rust側のロジックを使用してください
 * 代わりに `getPositionService().getInitialBoard()` を使用
 */
export function createInitialBoard(): BoardState {
    throw new Error(
        "createInitialBoard is deprecated. Use getPositionService().getInitialBoard() instead."
    );
}

/**
 * 初期盤面を非同期で取得（推奨）
 */
export async function createInitialBoardAsync(): Promise<BoardState> {
    return getPositionService().getInitialBoard();
}
```

#### 5.2 UIコンポーネントの更新

**ファイル**: `packages/ui/src/components/shogi-match.tsx`

```typescript
// Before
import { createInitialPositionState } from "@shogi/app-core";

// After
import { getPositionService } from "@shogi/app-core";

// 使用箇所
const [position, setPosition] = useState<PositionState | null>(null);

useEffect(() => {
    const initPosition = async () => {
        const service = getPositionService();
        const board = await service.getInitialBoard();
        setPosition({
            board,
            hands: { sente: {}, gote: {} },
            turn: "sente",
        });
    };
    initPosition();
}, []);
```

#### 5.3 棋譜インポート（loadMoves）の整合性

- `loadMoves` は `getPositionService().replayMovesStrict(sfen, moves)` を呼び出し、`applied` をそのまま `moves` ステートに採用する。
- 返却された `board` を表示盤面に反映し、`error` があればユーザー通知（トースト等）とし、それ以降の手は破棄する。
- 受け入れ条件: 不正手を含む棋譜をインポートしても盤面・持ち駒と `moves` が常に一致し、エクスポート/エンジン連携が同じ局面を指すこと（軽量統合テストまたは手動確認でも可）。

#### 5.4 合法手ハイライトの厳密化

- UIのマス/持ち駒ハイライトを `getPositionService().getLegalMoves(sfen, moves)` に接続し、エンジン（Rust Core）生成の合法手のみを表示する。
- 打ち歩詰め等の厳密判定はエンジン側の結果に委譲し、UIでは追加判定を行わない。
- 受け入れ条件: 現行の「選択マスと持ち駒のみハイライト」動作を置き換え、エンジン返却の合法手リストとハイライト表示が一致することを軽量統合テストまたは手動確認で検証する。

---

## 🧪 テスト戦略

### Rust Core テスト

**ファイル**: `packages/rust-core/crates/engine-core/src/position/json_conversion.rs`

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_initial_board_positions() {
        // 各駒の初期位置を検証
    }

    #[test]
    fn test_sfen_parse_accuracy() {
        // SFENパースの正確性
    }

    #[test]
    fn test_json_roundtrip() {
        // JSON変換の可逆性
    }
}
```

### Tauri Backend テスト

**ファイル**: `apps/desktop/src-tauri/tests/position_commands.rs` (新規作成)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_initial_board_command() {
        let result = get_initial_board();
        assert!(result.is_ok());
        let board = result.unwrap();
        assert_eq!(board.turn, "sente");
    }

    #[test]
    fn test_parse_sfen_command() {
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let result = parse_sfen_to_board(sfen.to_string());
        assert!(result.is_ok());
    }
}
```

### WASM テスト

**ファイル**: `packages/rust-core/crates/engine-wasm/tests/wasm_api.rs` (新規作成)

```rust
#[cfg(test)]
mod tests {
    use wasm_bindgen_test::*;

    #[wasm_bindgen_test]
    fn test_wasm_get_initial_board() {
        let result = wasm_get_initial_board();
        assert!(result.is_ok());
    }
}
```

### E2Eテスト

**ファイル**: `apps/desktop/src/__tests__/position-service.test.ts` (新規作成)

```typescript
import { describe, it, expect } from "vitest";
import { getPositionService } from "@shogi/app-core";

describe("PositionService", () => {
    it("should get initial board", async () => {
        const service = getPositionService();
        const board = await service.getInitialBoard();

        // 先手の飛車が2hにある
        expect(board["2h"]).toEqual({
            owner: "sente",
            type: "R",
        });

        // 先手の角が8hにある
        expect(board["8h"]).toEqual({
            owner: "sente",
            type: "B",
        });
    });

    it("should parse SFEN correctly", async () => {
        const service = getPositionService();
        const sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        const board = await service.parseSfen(sfen);

        expect(board["5i"]).toEqual({
            owner: "sente",
            type: "K",
        });
    });
});
```

---

## 📅 実装スケジュール

### Sprint 1: Rust Core実装（3-4日）

- [x] JSON型定義の作成
- [x] 変換関数の実装
- [x] ユニットテストの追加
- [x] ドキュメント作成

### Sprint 2: Backend拡張（2-3日）

- [x] Tauri commandの追加
- [x] WASM bindingの追加
- [x] 統合テストの追加

### Sprint 3: TypeScript統合（3-4日）

- [x] PositionServiceインターフェース作成
- [x] Tauri/WASM実装
- [x] ファクトリー関数実装
- [x] E2Eテスト追加

### Sprint 4: 既存コード移行（2-3日）

- [x] `createInitialBoard`のdeprecation
- [x] UIコンポーネント更新
- [x] 動作確認とバグ修正

### Sprint 5: クリーンアップ（1-2日）

- [x] 古いコードの削除
- [x] ドキュメント更新
- [x] パフォーマンステスト

**合計見積もり**: 11-16日

---

## 🔄 移行戦略

### 段階的移行アプローチ

#### Step 1: 新APIの追加（破壊的変更なし）
- Rust Core、Tauri、WASMに新しいAPIを追加
- 既存コードは動作し続ける

#### Step 2: 新APIの導入
- 新しい`PositionService`を使うコードを追加
- 旧APIと並行稼働

#### Step 3: 旧コードの置き換え
- 段階的に旧APIから新APIに移行
- 各コンポーネント単位でテスト

#### Step 4: 旧コードのdeprecation
- `@deprecated`アノテーションを追加
- 警告を表示

#### Step 5: 旧コードの削除
- 十分な移行期間後に削除
- メジャーバージョンアップ時

### 互換性の維持

```typescript
// 移行期間中の互換レイヤー
export function createInitialBoard(): BoardState {
    console.warn("createInitialBoard is deprecated. Use getPositionService().getInitialBoard()");

    // 同期的な呼び出しのため、キャッシュを返す
    if (!initialBoardCache) {
        throw new Error("Please use createInitialBoardAsync() or await initialization");
    }
    return initialBoardCache;
}

// 初期化時にキャッシュを用意
let initialBoardCache: BoardState | null = null;
getPositionService().getInitialBoard().then(board => {
    initialBoardCache = board;
});
```

---

## 🔙 ロールバック計画

### ロールバック条件

- 重大なバグが発見された場合
- パフォーマンスが著しく低下した場合
- Desktop/Webいずれかで動作しない場合

### ロールバック手順

1. **Git revert**
   ```bash
   git revert <commit-hash>
   ```

2. **feature flagによる切り替え**
   ```typescript
   const USE_RUST_POSITION_SERVICE = false; // ロールバック時にfalse

   export function getPositionService(): PositionService {
       if (!USE_RUST_POSITION_SERVICE) {
           return new LegacyPositionService();
       }
       // 新実装
   }
   ```

3. **段階的ロールバック**
   - まず問題のあるコンポーネントのみ旧実装に戻す
   - 安定化を確認後、全体をロールバック

---

## 📊 成功指標

### 機能要件

- ✅ Desktop/Webの両環境で動作
- ✅ 初期盤面が正確（飛車・角の位置が正しい）
- ✅ SFEN パース/生成が正確
- ✅ 合法手生成が正確
- ✅ 棋譜リプレイで不正手があっても盤面と手数が一致

### 非機能要件

- ✅ パフォーマンス低下なし（±5%以内）
- ✅ テストカバレッジ80%以上
- ✅ 既存機能の破壊なし

### 開発体験

- ✅ TypeScriptの型安全性向上
- ✅ コードの重複削減
- ✅ メンテナンスコスト削減

---

## 📝 チェックリスト

### Phase 1: Rust Core
- [ ] `types/json.rs` を作成
- [ ] `position/json_conversion.rs` を作成
- [ ] `lib.rs` に追加
- [ ] `replay_moves_strict` を実装し、適用済み手とエラーを返す
- [ ] ユニットテストを追加
- [ ] `cargo test` が通過
- [ ] `cargo clippy` が通過
- [ ] `cargo fmt` を実行

### Phase 2: Tauri Backend
- [ ] `get_initial_board` コマンド追加
- [ ] `parse_sfen_to_board` コマンド追加
- [ ] `board_to_sfen` コマンド追加
- [ ] `engine_replay_moves_strict` コマンド追加
- [ ] ハンドラーに登録
- [ ] テストを追加
- [ ] ビルド確認

### Phase 3: WASM Binding
- [ ] `wasm_get_initial_board` 追加
- [ ] `wasm_parse_sfen_to_board` 追加
- [ ] `wasm_board_to_sfen` 追加
- [ ] `wasm_get_legal_moves` 確認/追加
- [ ] `wasm_replay_moves_strict` 追加
- [ ] WASM テスト追加
- [ ] ビルド確認

### Phase 4: TypeScript統合
- [ ] `position-service.ts` 作成
- [ ] `tauri-position-service.ts` 作成
- [ ] `wasm-position-service.ts` 作成
- [ ] ファクトリー関数作成
- [ ] `replayMovesStrict` をPositionServiceで実装
- [ ] 型定義の同期
- [ ] E2Eテスト追加

### Phase 5: 移行
- [ ] `createInitialBoard` を deprecated
- [ ] UIコンポーネント更新
- [ ] `loadMoves` が `replayMovesStrict` の結果に同期する
- [ ] 合法手ハイライトを `getLegalMoves` 経由に変更
- [ ] 動作確認（Desktop）
- [ ] 動作確認（Web）
- [ ] パフォーマンステスト
- [ ] ドキュメント更新

---

## 🚨 注意事項

### Desktop/Web統一性の確保

> **重要**: すべての変更はDesktop（Tauri）とWeb（WASM）の両方で同時に実装する必要があります。

- PRレビュー時に両環境での動作を必ず確認
- CI/CDで両環境のテストを実行
- 片方だけの実装でマージしない

### 型定義の同期

Rust側のJSON型とTypeScript側の型は完全に一致させる：

```rust
// Rust
pub struct PieceJson {
    pub owner: String,
    #[serde(rename = "type")]
    pub piece_type: String,
    pub promoted: Option<bool>,
}
```

```typescript
// TypeScript
interface PieceJson {
    owner: string;
    type: string;
    promoted?: boolean;
}
```

### エラーハンドリング

- Rust側でのエラーは適切なメッセージを含める
- TypeScript側でエラーを適切にキャッチし、ユーザーに通知
- 開発環境では詳細なエラー情報を表示

---

## 📚 参考資料

- [Tauri Command Documentation](https://tauri.app/v1/guides/features/command)
- [wasm-bindgen Guide](https://rustwasm.github.io/wasm-bindgen/)
- [serde JSON](https://docs.rs/serde_json/)
- [SFEN Format Specification](http://shogidokoro.starfree.jp/usi.html)

---

## 🎯 次のステップ

1. **このドキュメントのレビュー**
   - チーム全体で実装計画を確認
   - 不明点や懸念事項の洗い出し

2. **新規セッションでの実装開始**
   - Phase 1から順次実装
   - 各Phaseごとにテストとレビュー

3. **定期的な進捗確認**
   - 週次で進捗を共有
   - 問題が発生したら早期にエスカレーション

---

**計画策定日**: 2025-12-09
**最終更新日**: 2025-12-09
**ステータス**: 📝 計画段階
