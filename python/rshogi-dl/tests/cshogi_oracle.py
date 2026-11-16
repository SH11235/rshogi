"""cshogi で局面を読む、engine の tokenizer と独立な参照実装 (テスト専用)。"""

from __future__ import annotations

import cshogi

from rshogi_dl.contract import (
    CONTEXT_BLACK_TO_MOVE,
    CONTEXT_WHITE_TO_MOVE,
    HAND_MAX_COUNTS,
    HAND_OFFSETS,
    OPPONENT_HAND_BASE,
    OWN_HAND_BASE,
)

from .support import label_from_usi

_PIECE_NAMES = (
    "PAWN",
    "LANCE",
    "KNIGHT",
    "SILVER",
    "BISHOP",
    "ROOK",
    "GOLD",
    "KING",
    "PROM_PAWN",
    "PROM_LANCE",
    "PROM_KNIGHT",
    "PROM_SILVER",
    "PROM_BISHOP",
    "PROM_ROOK",
)
# 駒 code -> (駒の色, 1..14 の駒種 ID)
_PIECES = {
    getattr(cshogi, prefix + name): (color, piece_id)
    for prefix, color in (("B", cshogi.BLACK), ("W", cshogi.WHITE))
    for piece_id, name in enumerate(_PIECE_NAMES, start=1)
}
_HAND_ORDER = (
    cshogi.HPAWN,
    cshogi.HLANCE,
    cshogi.HKNIGHT,
    cshogi.HSILVER,
    cshogi.HBISHOP,
    cshogi.HROOK,
    cshogi.HGOLD,
)


def _board(sfen: str) -> cshogi.Board:
    board = cshogi.Board()
    board.set_sfen(sfen)
    return board


def tokens_from_sfen(sfen: str) -> list[int]:
    board = _board(sfen)
    turn = board.turn
    white_to_move = turn == cshogi.WHITE
    squares = [0] * 81
    for index, piece in enumerate(board.pieces):
        if piece not in _PIECES:
            continue
        color, piece_id = _PIECES[piece]
        canonical = 80 - index if white_to_move else index
        squares[canonical] = piece_id if color == turn else 14 + piece_id
    hands = board.pieces_in_hand
    tokens = squares
    for base, color in ((OWN_HAND_BASE, turn), (OPPONENT_HAND_BASE, 1 - turn)):
        for offset, limit, hand_piece in zip(HAND_OFFSETS, HAND_MAX_COUNTS, _HAND_ORDER, strict=True):
            count = hands[color][hand_piece]
            if not 0 <= count <= limit:
                raise ValueError(f"hand count out of range: {count}")
            tokens.append(base + offset + count)
    tokens.append(CONTEXT_WHITE_TO_MOVE if white_to_move else CONTEXT_BLACK_TO_MOVE)
    return tokens


def legal_labels_from_sfen(sfen: str) -> dict[str, int]:
    """全合法手の USI 文字列 -> policy label。"""
    board = _board(sfen)
    white_to_move = board.turn == cshogi.WHITE
    return {
        usi: label_from_usi(usi, white_to_move)
        for usi in (cshogi.move_to_usi(move) for move in board.legal_moves)
    }
