from __future__ import annotations

import copy
import json
from pathlib import Path
import subprocess
import unittest

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper
import torch

from rshogi_dl.export import onnx_bytes
from rshogi_dl.model import TEST_CONFIG, SquareTransformer
from .support import HIRATE, artifact_tempdir, env_binary


class EngineContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.manual_seed(41)
        cls.token = onnx.load_from_string(onnx_bytes(SquareTransformer(TEST_CONFIG)))

    def setUp(self):
        self.probe = env_binary(self, 'RSHOGI_MODEL_PROBE_BIN')
        self.directory = artifact_tempdir()
        self.addCleanup(self.directory.cleanup)
        self.counter = 0

    def run_model(self, model, batch=1, legacy=False):
        self.counter += 1
        path = Path(self.directory.name) / f'model-{self.counter}.onnx'
        path.write_bytes(model.SerializeToString())
        binary = env_binary(self, 'RSHOGI_LEGACY_PROBE_BIN') if legacy else self.probe
        command = [binary, str(path), '-1']
        if not legacy:
            command.append(str(batch))
        return subprocess.run(command, input=HIRATE+'\n', text=True, capture_output=True, timeout=30)

    def rejected(self, model, **kwargs):
        result = self.run_model(model, **kwargs)
        self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_missing_unknown_or_conflicting_token_metadata_is_rejected(self):
        for key, value in [('rshogi.input_format', None), ('rshogi.input_format', 'unknown'), ('rshogi.input_format', ''),
                           ('rshogi.policy_format', None), ('rshogi.value_format', 'black-score')]:
            with self.subTest(key=key, value=value):
                model = copy.deepcopy(self.token)
                metadata = {entry.key: entry.value for entry in model.metadata_props}
                if value is None:
                    del metadata[key]
                else:
                    metadata[key] = value
                helper.set_model_props(model, metadata)
                self.rejected(model)

    def test_wrong_dtype_or_shape_is_rejected(self):
        model = copy.deepcopy(self.token)
        model.graph.input[0].type.tensor_type.elem_type = TensorProto.INT32
        self.rejected(model)
        model = copy.deepcopy(self.token)
        model.graph.input[0].type.tensor_type.shape.dim[1].dim_value = 95
        self.rejected(model)
        model = copy.deepcopy(self.token)
        model.graph.output[0].name = 'other_policy'
        self.rejected(model)

    def test_static_batch_one_is_only_accepted_for_max_batch_one(self):
        model = copy.deepcopy(self.token)
        for outlet in [*model.graph.input, *model.graph.output]:
            outlet.type.tensor_type.shape.dim[0].dim_value = 1
        result = self.run_model(model)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.rejected(model, batch=4)

    def test_legacy_api_rejects_token_model(self):
        self.rejected(self.token, legacy=True)

    def test_legacy_shapes_and_reserved_metadata(self):
        inputs = [helper.make_tensor_value_info('input1', TensorProto.FLOAT, [1, 62, 9, 9]),
                  helper.make_tensor_value_info('input2', TensorProto.FLOAT, [1, 57, 9, 9])]
        outputs = [helper.make_tensor_value_info('output_policy', TensorProto.FLOAT, [1, 2187]),
                   helper.make_tensor_value_info('output_value', TensorProto.FLOAT, [1, 1])]
        nodes = [helper.make_node('Constant', [], [name], value=numpy_helper.from_array(array))
                 for name, array in [('output_policy', np.zeros((1, 2187), np.float32)),
                                     ('output_value', np.full((1, 1), 0.5, np.float32))]]
        model = helper.make_model(helper.make_graph(nodes, 'basic', inputs, outputs),
                                  opset_imports=[helper.make_opsetid('', 18)], ir_version=10)
        for legacy in [False, True]:
            result = self.run_model(model, legacy=legacy)
            self.assertEqual(result.returncode, 0, result.stderr)
            output = json.loads(result.stdout)
            self.assertEqual(output['value'] if legacy else output['values'][0], 0.5)
        self.rejected(model, batch=4)
        helper.set_model_props(model, {'rshogi.value_format': 'black-score'})
        self.rejected(model)
        self.rejected(model, legacy=True)
