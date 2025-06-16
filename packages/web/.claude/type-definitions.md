# Type Definitions Reference

Quick reference for key types to avoid reading files repeatedly.

## Core Types (from "shogi-core")

### Domain Types
```typescript
// Square position on 9x9 board
type Square = { row: 1-9, column: 1-9 }
type SquareKey = "11" | "12" | ... | "99" // string representation

// Piece types
type PieceType = "pawn" | "lance" | "knight" | "silver" | "gold" | "bishop" | "rook" | "king"
type PieceTypeDefinition = "歩" | "香" | "桂" | "銀" | "金" | "角" | "飛" | "王" | "玉" | "と" | "成香" | "成桂" | "成銀" | "馬" | "龍"
type Player = "black" | "white"

type Piece = {
    type: PieceType
    owner: Player
    promoted: boolean
}

// Board state
type Board = Record<SquareKey, Piece | null>

// Move types
type NormalMove = {
    type: "move"
    from: Square
    to: Square
    piece: Piece
    promote: boolean
    captured: Piece | null
}

type DropMove = {
    type: "drop"
    to: Square
    piece: Piece
}

type Move = NormalMove | DropMove

// Game state
type GameStatus = "playing" | "check" | "checkmate" | "draw" | "sennichite" | "perpetual_check" | "timeout" | "resigned" | "black_win" | "white_win"

// Captured pieces
type Hands = {
    black: Record<PieceType, number>
    white: Record<PieceType, number>
}
```

## Game Store State
```typescript
type GameState = {
    board: Board
    currentPlayer: Player
    selectedSquare: Square | null
    validMoves: Square[]
    moveHistory: Move[]
    gameStatus: GameStatus
    hands: Hands
}
```

## Component Props

### Board Component
```typescript
type BoardProps = {
    board: Board
    selectedSquare: Square | null
    validMoves: Square[]
    onSquareClick: (square: Square) => void
}
```

### Piece Component
```typescript
type PieceProps = {
    piece: Piece
    square: Square
    isSelected?: boolean
    isValidMove?: boolean
    onClick?: () => void
}
```

### GameInfo Component
```typescript
type GameInfoProps = {
    currentPlayer: Player
    gameStatus: GameStatus
    moveCount: number
    onReset: () => void
}
```

### CapturedPieces Component
```typescript
type CapturedPiecesProps = {
    hands: Hands
    currentPlayer: Player
    onPieceDrop?: (pieceType: PieceType) => void
}
```

## Common Utility Types

```typescript
// For test helpers
type EmptyBoard = Record<SquareKey, null>
type PartialBoard = Partial<Board>

// For component variations
type ButtonVariant = "default" | "destructive" | "outline" | "secondary" | "ghost" | "link"
type ButtonSize = "default" | "sm" | "lg" | "icon"
```