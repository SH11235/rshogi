"""96 token を入力にする pre-norm Transformer encoder。"""

from __future__ import annotations

from dataclasses import asdict, dataclass

import torch
from torch import nn
from torch.nn import functional

from .contract import (
    CONTEXT_POSITION,
    POLICY_CHANNELS,
    POLICY_SIZE,
    SQUARE_COUNT,
    TOKEN_COUNT,
    VOCAB_SIZE,
)


@dataclass(frozen=True)
class ModelConfig:
    """モデルの大きさ。接続確認用の構成であり、強さや速度で選んだ値ではない。"""

    layers: int
    width: int
    heads: int
    ffn_width: int

    def __post_init__(self) -> None:
        for name, value in asdict(self).items():
            if type(value) is not int or value <= 0:
                raise ValueError(f"{name} must be a positive integer")
        if self.width % self.heads != 0:
            raise ValueError("width must be divisible by heads")

    def to_dict(self) -> dict[str, int]:
        """checkpoint と ONNX metadata に入れる dict を返す。"""
        return asdict(self)

    @classmethod
    def from_dict(cls, raw: object) -> ModelConfig:
        """:meth:`to_dict` の出力から復元する。過不足のある key は拒否する。"""
        if type(raw) is not dict or set(raw) != {"layers", "width", "heads", "ffn_width"}:
            raise ValueError("model config must have exactly layers, width, heads, ffn_width")
        return cls(**raw)


INITIAL_CONFIG = ModelConfig(layers=4, width=128, heads=4, ffn_width=512)
TEST_CONFIG = ModelConfig(layers=2, width=32, heads=4, ffn_width=128)
NAMED_CONFIGS = {"initial": INITIAL_CONFIG, "test": TEST_CONFIG}
ATTENTION_BACKENDS = ("explicit", "sdpa")


def flatten_policy(per_square: torch.Tensor) -> torch.Tensor:
    """``[B, 81, 27]`` (マスごとの channel) を ``81 * channel + マス`` の ``[B, 2187]`` に並べる。"""
    return per_square.transpose(1, 2).reshape(-1, POLICY_SIZE)


class SelfAttention(nn.Module):
    """全 token 間の双方向 multi-head attention。"""

    def __init__(self, width: int, heads: int, attention_backend: str = "explicit") -> None:
        super().__init__()
        if attention_backend not in ATTENTION_BACKENDS:
            raise ValueError(f"attention_backend must be one of {ATTENTION_BACKENDS}")
        self.attention_backend = attention_backend
        self.heads = heads
        self.head_width = width // heads
        self.scale = self.head_width**-0.5
        self.query = nn.Linear(width, width)
        self.key = nn.Linear(width, width)
        self.value = nn.Linear(width, width)
        self.output = nn.Linear(width, width)

    def _split_heads(self, x: torch.Tensor) -> torch.Tensor:
        # batch 軸だけを -1 にすると、export した graph に動的な shape 計算が入らない。
        return x.view(-1, TOKEN_COUNT, self.heads, self.head_width).transpose(1, 2)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        query = self._split_heads(self.query(x))
        key = self._split_heads(self.key(x))
        value = self._split_heads(self.value(x))
        # 配備 graph は実行方式に依存させず、標準 MatMul / Softmax の ONNX 契約を保つ。
        if self.attention_backend == "sdpa" and not torch.jit.is_tracing():
            attended = functional.scaled_dot_product_attention(
                query, key, value, dropout_p=0.0, is_causal=False, scale=self.scale
            )
        else:
            scores = torch.matmul(query, key.transpose(-2, -1)) * self.scale
            weights = torch.softmax(scores, dim=-1)
            attended = torch.matmul(weights, value)
        mixed = attended.transpose(1, 2)
        return self.output(mixed.reshape(-1, TOKEN_COUNT, self.heads * self.head_width))


class FeedForward(nn.Module):
    """位置ごとの 2 層 MLP。"""

    def __init__(self, width: int, ffn_width: int) -> None:
        super().__init__()
        self.expand = nn.Linear(width, ffn_width)
        self.activation = nn.GELU()
        self.project = nn.Linear(ffn_width, width)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.project(self.activation(self.expand(x)))


class EncoderBlock(nn.Module):
    """pre-LayerNorm の encoder block。"""

    def __init__(self, config: ModelConfig, attention_backend: str = "explicit") -> None:
        super().__init__()
        self.attention_norm = nn.LayerNorm(config.width, eps=1e-5)
        self.attention = SelfAttention(config.width, config.heads, attention_backend)
        self.ffn_norm = nn.LayerNorm(config.width, eps=1e-5)
        self.ffn = FeedForward(config.width, config.ffn_width)

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        x = x + self.attention(self.attention_norm(x))
        return x + self.ffn(self.ffn_norm(x))


class SquareTransformer(nn.Module):
    """token 列から policy logit と value logit を出す。

    policy はマス token 0..80 に共有の線形層を掛け、value は最後の token (元の手番) から読む。
    """

    def __init__(self, config: ModelConfig, *, attention_backend: str = "explicit") -> None:
        super().__init__()
        self.config = config
        self.attention_backend = attention_backend
        self.token_embedding = nn.Embedding(VOCAB_SIZE, config.width)
        self.position_embedding = nn.Parameter(torch.empty(TOKEN_COUNT, config.width))
        self.blocks = nn.ModuleList(
            EncoderBlock(config, attention_backend) for _ in range(config.layers)
        )
        self.final_norm = nn.LayerNorm(config.width, eps=1e-5)
        self.policy_head = nn.Linear(config.width, POLICY_CHANNELS)
        self.value_head = nn.Sequential(
            nn.Linear(config.width, config.width),
            nn.GELU(),
            nn.Linear(config.width, 1),
        )
        nn.init.normal_(self.token_embedding.weight, std=0.02)
        nn.init.normal_(self.position_embedding, std=0.02)

    def forward(self, tokens: torch.Tensor) -> tuple[torch.Tensor, torch.Tensor]:
        """``tokens`` は int64 の ``[B, 96]``。``([B, 2187] policy logit, [B, 1] value logit)`` を返す。"""
        if not torch.jit.is_tracing():
            if tokens.dtype != torch.int64:
                raise ValueError("tokens must be int64")
            if tokens.dim() != 2 or tokens.shape[1] != TOKEN_COUNT:
                raise ValueError(f"tokens must have shape [B, {TOKEN_COUNT}]")
        x = self.token_embedding(tokens) + self.position_embedding
        for block in self.blocks:
            x = block(x)
        x = self.final_norm(x)
        policy = flatten_policy(self.policy_head(x[:, :SQUARE_COUNT]))
        value = self.value_head(x[:, CONTEXT_POSITION])
        return policy, value

    def parameter_groups(self) -> dict[str, list[nn.Parameter]]:
        """勾配と更新を確認する単位で parameter を分ける。全 parameter をちょうど 1 回ずつ含む。"""
        attention: list[nn.Parameter] = []
        ffn: list[nn.Parameter] = []
        norm: list[nn.Parameter] = list(self.final_norm.parameters())
        for block in self.blocks:
            attention.extend(block.attention.parameters())
            ffn.extend(block.ffn.parameters())
            norm.extend(block.attention_norm.parameters())
            norm.extend(block.ffn_norm.parameters())
        return {
            "token_embedding": [self.token_embedding.weight],
            "position_embedding": [self.position_embedding],
            "attention": attention,
            "ffn": ffn,
            "norm": norm,
            "policy_head": list(self.policy_head.parameters()),
            "value_head": list(self.value_head.parameters()),
        }


class ExportModule(nn.Module):
    """ONNX 用の出力。学習時は logit のまま扱う value に sigmoid を付ける。"""

    def __init__(self, model: SquareTransformer) -> None:
        super().__init__()
        self.model = model

    def forward(self, tokens: torch.Tensor) -> tuple[torch.Tensor, torch.Tensor]:
        policy, value_logit = self.model(tokens)
        return policy, torch.sigmoid(value_logit)
