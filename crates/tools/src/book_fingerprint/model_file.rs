//! 重みを展開せず、既知の自己完結 ONNX 構造と同じ byte 列の指紋を確認する。

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
enum Message {
    Model,
    Graph,
    Node,
    Attribute,
    Tensor,
}

fn known_operator(name: &[u8]) -> bool {
    matches!(
        name,
        b"Add"
            | b"AveragePool"
            | b"BatchNormalization"
            | b"Cast"
            | b"Concat"
            | b"Constant"
            | b"Conv"
            | b"Div"
            | b"Erf"
            | b"Flatten"
            | b"Gather"
            | b"Gemm"
            | b"GlobalAveragePool"
            | b"Identity"
            | b"LayerNormalization"
            | b"MatMul"
            | b"MaxPool"
            | b"Mul"
            | b"Pow"
            | b"ReduceMean"
            | b"Relu"
            | b"Reshape"
            | b"Shape"
            | b"Sigmoid"
            | b"Slice"
            | b"Softmax"
            | b"Sqrt"
            | b"Sub"
            | b"Tanh"
            | b"Transpose"
            | b"Unsqueeze"
    )
}

struct Reader {
    file: BufReader<File>,
    hash: Sha256,
    position: u64,
    scratch: [u8; 65536],
}

impl Read for Reader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.file.read(output)?;
        self.hash.update(&output[..count]);
        self.position =
            self.position.checked_add(count as u64).ok_or(io::ErrorKind::InvalidData)?;
        Ok(count)
    }
}

impl Reader {
    fn read_scratch(&mut self, limit: usize) -> io::Result<usize> {
        let count = self.file.read(&mut self.scratch[..limit])?;
        self.hash.update(&self.scratch[..count]);
        self.position =
            self.position.checked_add(count as u64).ok_or(io::ErrorKind::InvalidData)?;
        Ok(count)
    }

    fn varint(&mut self, end: u64) -> io::Result<u64> {
        let mut value = 0;
        for shift in (0..70).step_by(7) {
            if self.position >= end {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let mut byte = [0];
            self.read_exact(&mut byte)?;
            if shift == 63 && byte[0] > 1 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            value |= u64::from(byte[0] & 127) << shift;
            if byte[0] < 128 {
                return Ok(value);
            }
        }
        Err(io::ErrorKind::InvalidData.into())
    }

    fn skip(&mut self, count: u64, end: u64) -> io::Result<()> {
        if count > end - self.position {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        let mut remaining = count;
        while remaining != 0 {
            let limit = remaining.min(self.scratch.len() as u64) as usize;
            let count = self.read_scratch(limit)?;
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            remaining -= count as u64;
        }
        Ok(())
    }

    fn field_end(&mut self, end: u64) -> io::Result<u64> {
        let length = self.varint(end)?;
        if length > end - self.position {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(self.position + length)
    }

    fn message(&mut self, kind: Message, end: u64, depth: usize) -> io::Result<bool> {
        if depth > 5 {
            return Ok(false);
        }
        let mut ir_version = false;
        let mut graph = false;
        let mut operator = false;
        while self.position < end {
            let tag = self.varint(end)?;
            let field = tag >> 3;
            let wire = tag & 7;
            let child = match (kind, field) {
                (Message::Model, 7) if !graph => {
                    graph = true;
                    Some(Message::Graph)
                }
                (Message::Graph, 1) => Some(Message::Node),
                (Message::Graph, 5) | (Message::Attribute, 5 | 10) => Some(Message::Tensor),
                (Message::Node, 5) => Some(Message::Attribute),
                _ => None,
            };
            if let Some(child) = child {
                if wire != 2 {
                    return Ok(false);
                }
                let child_end = self.field_end(end)?;
                if !self.message(child, child_end, depth + 1)? {
                    return Ok(false);
                }
                continue;
            }
            let allowed = match kind {
                Message::Model => matches!((field, wire), (1 | 5, 0) | (2..=4 | 6 | 8 | 14, 2)),
                // ValueInfo / metadata / opset は型と文字列だけで TensorProto を含まない。
                Message::Graph => matches!((field, wire), (2 | 10..=13, 2)),
                Message::Node => matches!((field, wire), (1..=4 | 6 | 7, 2)),
                Message::Attribute => matches!(
                    (field, wire),
                    (1 | 4 | 9 | 13, 2) | (2 | 7, 5) | (7 | 8, 2) | (3 | 8 | 20, 0)
                ),
                Message::Tensor => matches!(
                    (field, wire),
                    (1 | 2 | 5 | 7 | 11 | 14, 0) | (1 | 4..=12, 2) | (4, 5) | (10, 1)
                ),
            };
            if !allowed {
                return Ok(false);
            }
            match wire {
                0 => {
                    let value = self.varint(end)?;
                    match (kind, field) {
                        (Message::Model, 1) => {
                            if ir_version || !(1..=13).contains(&value) {
                                return Ok(false);
                            }
                            ir_version = true;
                        }
                        (Message::Tensor, 14) if value != 0 => return Ok(false),
                        (Message::Attribute, 20) if !matches!(value, 1..=4 | 6..=9) => {
                            return Ok(false);
                        }
                        _ => {}
                    }
                }
                1 => self.skip(8, end)?,
                2 => {
                    let field_end = self.field_end(end)?;
                    if matches!((kind, field), (Message::Node, 4)) {
                        let length = field_end - self.position;
                        if operator || length > 32 {
                            return Ok(false);
                        }
                        let mut name = [0; 32];
                        self.read_exact(&mut name[..length as usize])?;
                        if !known_operator(&name[..length as usize]) {
                            return Ok(false);
                        }
                        operator = true;
                    } else if matches!((kind, field), (Message::Node, 7)) {
                        let length = field_end - self.position;
                        if length > 7 {
                            return Ok(false);
                        }
                        let mut domain = [0; 7];
                        self.read_exact(&mut domain[..length as usize])?;
                        if length != 0 && &domain[..length as usize] != b"ai.onnx" {
                            return Ok(false);
                        }
                    } else {
                        self.skip(field_end - self.position, end)?;
                    }
                }
                5 => self.skip(4, end)?,
                _ => return Ok(false),
            }
        }
        Ok(match kind {
            Message::Model => ir_version && graph,
            Message::Node => operator,
            _ => true,
        })
    }
}

pub(super) fn identify(path: &Path) -> io::Result<(String, bool)> {
    let file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut hash = Sha256::new();
    hash.update(b"file");
    let mut reader = Reader {
        file: BufReader::with_capacity(65536, file),
        hash,
        position: 0,
        scratch: [0; 65536],
    };
    let onnx = path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("onnx"));
    let complete = onnx && reader.message(Message::Model, length, 1).unwrap_or(false);
    while reader.read_scratch(65536)? != 0 {}
    Ok((format!("{:x}", reader.hash.finalize()), complete && reader.position == length))
}

#[cfg(test)]
pub(super) fn fixture(weights: &[u8]) -> Vec<u8> {
    fn bytes(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut output = vec![tag];
        let mut length = value.len();
        while length >= 128 {
            output.push((length as u8 & 127) | 128);
            length >>= 7;
        }
        output.push(length as u8);
        output.extend_from_slice(value);
        output
    }
    let mut tensor = vec![8, 1, 16, 1];
    tensor.extend(bytes(74, weights));
    let graph = bytes(42, &tensor);
    let mut model = vec![8, 10];
    model.extend(bytes(58, &graph));
    model
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(number: u8, value: &[u8]) -> Vec<u8> {
        assert!(value.len() < 128 && number < 16);
        let mut output = vec![number * 8 + 2, value.len() as u8];
        output.extend(value);
        output
    }

    fn model(graph: &[u8]) -> Vec<u8> {
        let mut bytes = vec![8, 10];
        bytes.extend(field(7, graph));
        bytes
    }

    fn node(attribute: &[u8]) -> Vec<u8> {
        let mut node = field(4, b"Constant");
        node.extend(field(5, attribute));
        field(1, &node)
    }

    fn prove(bytes: &[u8]) -> bool {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.onnx");
        std::fs::write(&path, bytes).unwrap();
        let (hash, complete) = identify(&path).unwrap();
        assert_eq!(hash, super::super::content_digest(&path).unwrap());
        complete
    }

    #[test]
    fn raw_weights_are_streamed_and_hashed_without_interpreting_their_bytes() {
        let weights: Vec<_> = (0..200000).map(|index| index as u8).collect();
        assert!(prove(&fixture(&weights)));
        assert!(prove(&model(&node(&field(5, &[16, 1, 74, 1, 0])))));
        assert!(prove(&model(&node(&field(5, &[16, 1, 37, 0, 0, 128, 63, 37, 0, 0, 0, 64])))));
        assert!(!prove(&model(&node(&field(5, &[16, 1, 37, 0, 0, 0])))));
    }

    #[test]
    fn unsupported_resource_containers_and_external_tensors_fail_closed() {
        for graph in [
            field(5, &[112, 1]),
            field(5, &[106, 0]),
            field(15, &[]),
            node(&field(6, &[])),
            node(&field(11, &[])),
            node(&field(5, &[112, 1])),
            {
                let mut bytes = field(4, b"Constant");
                bytes.extend([82, 0]);
                field(1, &bytes)
            },
            {
                let mut bytes = field(4, b"Constant");
                bytes.extend(field(7, b"custom"));
                field(1, &bytes)
            },
            field(1, &field(4, b"CustomLoader")),
        ] {
            assert!(!prove(&model(&graph)), "{graph:?}");
        }
        for extra in [[162, 1, 0], [202, 1, 0], [210, 1, 0]] {
            let mut bytes = model(&[]);
            bytes.extend(extra);
            assert!(!prove(&bytes));
        }
    }

    #[test]
    fn malformed_lengths_varints_wire_types_versions_and_truncation_are_not_proof() {
        let valid = fixture(&[0, 0, 128, 63]);
        for end in 0..valid.len() {
            assert!(!prove(&valid[..end]), "prefix {end}");
        }
        for bytes in [
            vec![8, 0, 58, 0],
            vec![8, 14, 58, 0],
            vec![8, 10, 8, 10, 58, 0],
            vec![8, 10, 58, 127],
            vec![8, 10, 59],
            vec![128; 11],
            vec![8, 255, 255, 255, 255, 255, 255, 255, 255, 255, 2],
            vec![8, 10, 58, 0, 58, 0],
        ] {
            assert!(!prove(&bytes), "{bytes:?}");
        }
    }

    #[test]
    #[ignore = "RSHOGI_ONNX_MODELS に検証対象の export 済みモデルを指定する"]
    fn canonical_exports_are_proven_and_hashed_from_the_same_stream() {
        let paths = std::env::var_os("RSHOGI_ONNX_MODELS").expect("RSHOGI_ONNX_MODELS");
        let paths: Vec<_> = std::env::split_paths(&paths).collect();
        assert!(!paths.is_empty());
        for path in paths {
            let (hash, complete) = identify(&path).unwrap();
            assert!(complete, "{}", path.display());
            assert_eq!(hash, super::super::content_digest(&path).unwrap());
        }
    }
}
