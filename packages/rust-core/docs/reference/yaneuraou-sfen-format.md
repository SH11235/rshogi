# YaneuraOu SFEN File Format Documentation

## Overview

YaneuraOu SFEN (Shogi Forsyth-Edwards Notation) files are text-based databases used for storing opening book positions and their evaluations. The format consists of a header followed by position entries with move variations.

- ヘッダー: #YANEURAOU-DB2016 1.00
- 各局面: sfen で始まる行に局面情報
- 指し手: 各局面に対する候補手と評価値
- 記法: 通常の移動（8i7g）、成り（3d3c+）、打ち（P*6g）
- 評価値: 手番側有利が正の値

## File Structure

### Header
```
#YANEURAOU-DB2016 1.00
```
- Format identifier: `#YANEURAOU-DB2016`
- Version number: `1.00`

### Position Entries

Each position entry follows this structure:

```
sfen <position> <turn> <hand> <move_count>
<move1> <type1> <eval1> <depth1> <nodes1>
<move2> <type2> <eval2> <depth2> <nodes2>
...
```

#### Position Line Format
- `sfen`: Keyword indicating a position entry
- `<position>`: Board position in SFEN notation (9x9 grid representation)
- `<turn>`: Current player (`b` for black/先手, `w` for white/後手)
- `<hand>`: Pieces in hand (captured pieces available for dropping)
- `<move_count>`: Number of moves played from initial position

#### Move Line Format
Each move option for a position contains:
- `<move>`: Move notation (e.g., `8i7g`, `3d3c+`, `P*6g`)
- `<type>`: Move type (commonly `none`)
- `<eval>`: Position evaluation (positive favors current player)
- `<depth>`: Search depth used for evaluation
- `<nodes>`: Number of nodes searched during evaluation

## Move Notation

### Normal Moves
- Format: `<from><to>[+]`
- Example: `8i7g` (move from 8i to 7g)
- Example: `3d3c+` (move from 3d to 3c with promotion)

### Drop Moves
- Format: `<piece>*<square>`
- Example: `P*6g` (drop pawn at 6g)
- Example: `N*4e` (drop knight at 4e)

### Piece Notation
- `P`: Pawn (歩)
- `N`: Knight (桂)
- `L`: Lance (香)
- `S`: Silver (銀)
- `G`: Gold (金)
- `B`: Bishop (角)
- `R`: Rook (飛)
- `K`: King (王/玉)

## SFEN Position Format

### Board Representation
The board is represented as a string with ranks separated by `/`:
- Empty squares: Numbers (1-9 indicating consecutive empty squares)
- Pieces: Letters (uppercase for unpromoted, lowercase for promoted)
- Promoted pieces: `+` prefix (e.g., `+B` for promoted bishop)

### Example Position
```
sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0
```

### Hand Notation
Pieces in hand are listed after the position:
- Format: `<piece><count>` (count omitted if 1)
- Example: `NLP2sl2p` = 1 Knight, 1 Lance, 1 Pawn, 2 Silver, 1 Lance, 2 Pawns

## Evaluation System

### Evaluation Values
- Positive values: Favor the current player
- Negative values: Favor the opponent
- Range: Typically -9999 to +9999 (centipawns)

### Depth and Nodes
- `depth`: Search depth (0 = book move, >0 = engine analysis)
- `nodes`: Number of positions evaluated during search

## Example Entry

```
sfen +B+B1Sg2nl/5kg2/4p2p1/3pspP1p/p6PP/1p1rP4/P1p2P2N/1PG1G4/LNK5R b NLP2sl2p 0
8i7g none 194 3 0
3d3c+ none -203 0 0
7h7g none -325 0 0
8a5d none -854 0 0
```

This represents:
- A specific board position with black to move
- Four possible moves with their evaluations
- `8i7g` is the best move (+194 evaluation, depth 3)
- Other moves have negative evaluations (worse for black)

## Usage Notes

- Files use CRLF line endings
- Each position can have multiple move variations
- Evaluations are from the perspective of the player to move
- Depth 0 typically indicates book moves rather than engine analysis
- The format is designed for fast lookup of opening positions and their best continuations
