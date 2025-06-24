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

## Timer Types

```typescript
// Timer configuration
type TimerMode = "basic" | "fischer" | "perMove" | null
type WarningLevel = "normal" | "low" | "critical" | "byoyomi"

type TimerConfig = {
    mode: TimerMode
    basicTime: number  // seconds
    byoyomiTime: number  // seconds
    fischerIncrement: number  // seconds
    perMoveLimit: number  // seconds
}

type TimerState = {
    config: TimerConfig
    blackTime: number  // milliseconds
    whiteTime: number  // milliseconds
    blackInByoyomi: boolean
    whiteInByoyomi: boolean
    activePlayer: Player | null
    isPaused: boolean
    lastTickTime: number
    blackWarningLevel: WarningLevel
    whiteWarningLevel: WarningLevel
    hasTimedOut: boolean
    timedOutPlayer: Player | null
}

type TimerActions = {
    initializeTimer: (config: TimerConfig) => void
    startPlayerTimer: (player: Player) => void
    switchTimer: () => void
    pauseTimer: () => void
    resumeTimer: () => void
    resetTimer: () => void
    tick: () => void
    updateWarningLevels: () => void
}
```

## Audio Types

```typescript
// Sound types
type SoundType = "piece" | "check" | "gameEnd"
type VolumeLevel = 0 | 25 | 50 | 75 | 100

type SoundConfig = {
    path: string
    defaultVolume?: VolumeLevel
    preload?: boolean
}

type AudioPlayerState = {
    isInitialized: boolean
    volume: VolumeLevel
    isMuted: boolean
    loadedSounds: Set<SoundType>
}

type PlayOptions = {
    volume?: number  // 0-1 range
    playbackRate?: number
    interrupt?: boolean
}

type AudioManager = {
    initialize: () => Promise<void>
    play: (type: SoundType, options?: PlayOptions) => Promise<void>
    setVolume: (volume: VolumeLevel) => void
    setMuted: (muted: boolean) => void
    preload: (type: SoundType) => Promise<void>
    stopAll: () => void
    getState: () => AudioPlayerState
}
```

## Settings Types

```typescript
// Time and volume settings
type TimeControlMinutes = 1 | 3 | 5 | 10 | 15 | 30 | 60 | 90
type ByoyomiSeconds = 0 | 10 | 30 | 60
type Theme = "light" | "dark" | "auto"

type TimeControlSettings = {
    mainTimeMinutes: TimeControlMinutes
    byoyomiSeconds: ByoyomiSeconds
    enabled: boolean
}

type AudioSettings = {
    masterVolume: VolumeLevel
    pieceSound: boolean
    checkSound: boolean
    gameEndSound: boolean
}

type DisplaySettings = {
    theme: Theme
    animations: boolean
    showValidMoves: boolean
    showLastMove: boolean
}

type GameSettings = {
    timeControl: TimeControlSettings
    audio: AudioSettings
    display: DisplaySettings
}
```

## UI State Types

```typescript
// UI-specific states
type PromotionPendingState = {
    from: Square
    to: Square
    piece: Piece
}

type SelectedDropPiece = {
    type: PieceType
    player: Player
}

type UIGameState = {
    selectedSquare: Square | null
    selectedDropPiece: SelectedDropPiece | null
    validMoves: Square[]
    validDropSquares: Square[]
    promotionPending: PromotionPendingState | null
}

// UI action callbacks
type UIActions = {
    onSquareSelect: (square: Square) => void
    onDropPieceSelect: (pieceType: PieceType, player: Player) => void
    onPromotionConfirm: (promote: boolean) => void
    onPromotionCancel: () => void
}

type GameActions = {
    onReset: () => void
    onResign: () => void
    onImportGame: (kifContent: string) => void
}

type HistoryActions = {
    onUndo: () => void
    onRedo: () => void
    onGoToMove: (moveIndex: number) => void
}
```