"""engine と共有する入出力契約の定数。

token ID・policy の並び・ONNX metadata は engine 側の実装と一致している必要がある。
値を変えると既存の ONNX と engine の組が壊れるので、互換でない変更は形式名ごと改める。
"""

from __future__ import annotations

INPUT_FORMAT = "stm-tokens-v1"
POLICY_FORMAT = "direction-destination-2187-v1"
VALUE_FORMAT = "stm-expected-score-sigmoid-v1"
ARCHITECTURE = "square-transformer-v1"
TRAINING_STATUS = "validation-only"

METADATA_INPUT_FORMAT = "rshogi.input_format"
METADATA_POLICY_FORMAT = "rshogi.policy_format"
METADATA_VALUE_FORMAT = "rshogi.value_format"
METADATA_ARCHITECTURE = "rshogi.architecture"
METADATA_MODEL_CONFIG = "rshogi.model_config"
METADATA_TRAINING_STATUS = "rshogi.training_status"

ONNX_OPSET = 18
ONNX_INPUT_NAME = "tokens"
ONNX_POLICY_NAME = "output_policy"
ONNX_VALUE_NAME = "output_value"

TOKEN_COUNT = 96
VOCAB_SIZE = 121
SQUARE_COUNT = 81
CONTEXT_POSITION = 95

# マスの ID: 0 = 空、1..14 = 手番側の駒、15..28 = 相手側の駒。
SQUARE_ID_MAX = 28

# 持駒 token の並びは policy の打ち channel と同じ 歩 香 桂 銀 角 飛 金。
HAND_TYPE_COUNT = 7
HAND_MAX_COUNTS = (18, 4, 4, 4, 2, 2, 4)
HAND_OFFSETS = (0, 19, 24, 29, 34, 37, 40)
OWN_HAND_POSITION = 81
OPPONENT_HAND_POSITION = 88
OWN_HAND_BASE = 29
OPPONENT_HAND_BASE = 74

# 最後の token は元の手番 (先手 / 後手) を表す。
CONTEXT_BLACK_TO_MOVE = 119
CONTEXT_WHITE_TO_MOVE = 120

POLICY_CHANNELS = 27
POLICY_SIZE = POLICY_CHANNELS * SQUARE_COUNT

# 将棋の 1 局面の合法手数の上限。
MAX_LEGAL_LABELS = 593

CPU_PARITY_RTOL = 1e-4
CPU_PARITY_ATOL = 1e-5
CUDA_PARITY_RTOL = 1e-3
CUDA_PARITY_ATOL = 1e-4
PARITY_BATCH_SIZES = (1, 2, 4)


def token_id_range(position: int) -> tuple[int, int]:
    """token 位置ごとに許される ID の範囲 (両端を含む) を返す。"""
    if 0 <= position < SQUARE_COUNT:
        return (0, SQUARE_ID_MAX)
    if OWN_HAND_POSITION <= position < OWN_HAND_POSITION + HAND_TYPE_COUNT:
        hand_type = position - OWN_HAND_POSITION
        low = OWN_HAND_BASE + HAND_OFFSETS[hand_type]
        return (low, low + HAND_MAX_COUNTS[hand_type])
    if OPPONENT_HAND_POSITION <= position < OPPONENT_HAND_POSITION + HAND_TYPE_COUNT:
        hand_type = position - OPPONENT_HAND_POSITION
        low = OPPONENT_HAND_BASE + HAND_OFFSETS[hand_type]
        return (low, low + HAND_MAX_COUNTS[hand_type])
    if position == CONTEXT_POSITION:
        return (CONTEXT_BLACK_TO_MOVE, CONTEXT_WHITE_TO_MOVE)
    raise ValueError(f"token position out of range: {position}")


TOKEN_ID_RANGES = tuple(token_id_range(position) for position in range(TOKEN_COUNT))
