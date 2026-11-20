"""CUDA Graph replay for fixed-size batches with policy targets on every row."""

from collections.abc import Callable
from dataclasses import fields, replace

import torch
from torch.nn import functional

from .data import Batch


_WARMUP_STEPS = 4
_LEGAL_WIDTH = 512
_LEGAL_FIELDS = ("legal_index", "legal_mask", "policy_target")


class StepGraph:
    def __init__(
        self,
        batch_size: int,
        device: torch.device,
        update: Callable[[Batch, bool], None],
        transfer: Callable[[Batch, torch.device], Batch],
        parameters: tuple[torch.nn.Parameter, ...],
        *,
        prefetch_input: bool = False,
    ) -> None:
        self._batch_size = batch_size
        self._device = device
        self._update = update
        self._transfer = transfer
        self._parameters = parameters
        self._warm_stream = torch.cuda.Stream(device)
        self._warm_steps = 0
        self._graph: torch.cuda.CUDAGraph | None = None
        self._static: Batch | None = None
        self._gradients: tuple[torch.Tensor | None, ...] = ()
        self._prefetch_input = prefetch_input
        self._input_frames: list[tuple[dict[str, torch.Tensor], torch.cuda.Event]] = []
        self.replays = 0

    def try_update(self, batch: Batch) -> bool:
        rows = batch.policy_rows
        width = batch.legal_index.shape[1]
        if (
            batch.tokens.shape[0] != self._batch_size or rows is None
            or rows.numel() != self._batch_size or width > _LEGAL_WIDTH
        ):
            return False
        if width < _LEGAL_WIDTH:
            batch = replace(batch, **{
                name: functional.pad(getattr(batch, name), (0, _LEGAL_WIDTH - width))
                for name in _LEGAL_FIELDS
            })
        if self._warm_steps < _WARMUP_STEPS:
            current = torch.cuda.current_stream(self._device)
            self._warm_stream.wait_stream(current)
            with torch.cuda.stream(self._warm_stream):
                self._update(self._transfer(batch, self._device), True)
            current.wait_stream(self._warm_stream)
            self._warm_steps += 1
        else:
            if self._graph is None:
                self._static = self._transfer(batch, self._device)
                self._graph = torch.cuda.CUDAGraph()
                with (
                    torch.autograd.set_multithreading_enabled(False),
                    torch.cuda.graph(self._graph, stream=self._warm_stream),
                ):
                    self._update(self._static, True)
                # Eager fallback can replace .grad while replay still uses its captured buffers.
                self._gradients = tuple(parameter.grad for parameter in self._parameters)
            else:
                self._copy_inputs(batch)
            self._graph.replay()
            self.replays += 1
        return True

    def _copy_inputs(self, batch: Batch) -> None:
        if not self._prefetch_input:
            for field in fields(Batch):
                getattr(self._static, field.name).copy_(getattr(batch, field.name))
            return
        if not self._input_frames:
            self._input_frames = [
                ({field.name: torch.empty_like(getattr(batch, field.name), pin_memory=True)
                  for field in fields(Batch)}, torch.cuda.Event())
                for _ in range(2)
            ]
        frame, copied = self._input_frames[(self.replays - 1) % len(self._input_frames)]
        # A host buffer can be refilled only after its previous device copy completes.
        if self.replays > len(self._input_frames):
            copied.synchronize()
        for name, host in frame.items():
            host.copy_(getattr(batch, name))
            getattr(self._static, name).copy_(host, non_blocking=True)
        copied.record(torch.cuda.current_stream(self._device))
