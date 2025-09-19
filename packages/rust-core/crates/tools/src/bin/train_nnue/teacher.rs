use std::cell::RefCell;
use std::fmt;
use std::path::Path;

use crate::model::SingleNetwork;
use crate::types::{TeacherKind, TeacherValueDomain};
use anyhow::Error;
use engine_core::evaluation::nnue::features::flip_us_them;
use tools::classic_roundtrip::{ClassicFp32Network, LayerOutputs};

#[derive(Debug)]
pub enum TeacherError {
    Io(std::io::Error),
    Load(Error),
    UnsupportedDomain {
        kind: TeacherKind,
        domain: TeacherValueDomain,
    },
    LayersUnavailable {
        kind: TeacherKind,
    },
}

impl fmt::Display for TeacherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TeacherError::Io(e) => write!(f, "{e}"),
            TeacherError::Load(e) => write!(f, "{e}"),
            TeacherError::UnsupportedDomain { kind, domain } => {
                write!(f, "teacher {:?} does not support domain {:?}", kind, domain)
            }
            TeacherError::LayersUnavailable { kind } => {
                write!(f, "teacher {:?} does not expose intermediate layers", kind)
            }
        }
    }
}

impl std::error::Error for TeacherError {}

impl From<std::io::Error> for TeacherError {
    fn from(value: std::io::Error) -> Self {
        TeacherError::Io(value)
    }
}

impl From<Error> for TeacherError {
    fn from(value: Error) -> Self {
        TeacherError::Load(value)
    }
}

#[derive(Clone, Debug)]
pub struct TeacherLayers {
    pub ft: Vec<f32>,
    pub h1: Vec<f32>,
    pub h2: Vec<f32>,
    pub out: f32,
}

#[derive(Clone, Debug)]
pub struct TeacherEval {
    pub value: f32,
    pub layers: Option<TeacherLayers>,
    pub domain: TeacherValueDomain,
}

#[derive(Clone, Copy, Debug)]
pub struct TeacherBatchRequest<'a> {
    pub features: &'a [u32],
}

pub trait TeacherNetwork: Send {
    fn kind(&self) -> TeacherKind;

    fn supports_domain(&self, domain: TeacherValueDomain) -> bool;

    fn evaluate_batch<'a>(
        &self,
        batch: &[TeacherBatchRequest<'a>],
        domain: TeacherValueDomain,
        capture_layers: bool,
    ) -> Result<Vec<TeacherEval>, TeacherError>;
}

pub fn load_teacher(
    path: &Path,
    kind: TeacherKind,
) -> Result<Box<dyn TeacherNetwork>, TeacherError> {
    match kind {
        TeacherKind::Single => {
            let net = SingleNetwork::load(path)?;
            Ok(Box::new(SingleTeacher::new(net)))
        }
        TeacherKind::ClassicFp32 => {
            let net = ClassicFp32Network::load(path)?;
            Ok(Box::new(ClassicFp32Teacher::new(net)))
        }
    }
}

struct SingleTeacher {
    net: SingleNetwork,
    acc: RefCell<Vec<f32>>,
    act: RefCell<Vec<f32>>,
}

impl SingleTeacher {
    fn new(net: SingleNetwork) -> Self {
        let acc = RefCell::new(Vec::with_capacity(net.acc_dim));
        let act = RefCell::new(Vec::with_capacity(net.acc_dim));
        Self { net, acc, act }
    }
}

impl TeacherNetwork for SingleTeacher {
    fn kind(&self) -> TeacherKind {
        TeacherKind::Single
    }

    fn supports_domain(&self, domain: TeacherValueDomain) -> bool {
        matches!(domain, TeacherValueDomain::WdlLogit)
    }

    fn evaluate_batch<'a>(
        &self,
        batch: &[TeacherBatchRequest<'a>],
        domain: TeacherValueDomain,
        capture_layers: bool,
    ) -> Result<Vec<TeacherEval>, TeacherError> {
        if capture_layers {
            return Err(TeacherError::LayersUnavailable { kind: self.kind() });
        }
        if !batch.is_empty() && !self.supports_domain(domain) {
            return Err(TeacherError::UnsupportedDomain {
                kind: self.kind(),
                domain,
            });
        }

        let mut results = Vec::with_capacity(batch.len());
        let mut acc = self.acc.borrow_mut();
        let mut act = self.act.borrow_mut();
        for req in batch {
            acc.resize(self.net.acc_dim, 0.0);
            act.resize(self.net.acc_dim, 0.0);
            let value = self.net.forward_with_buffers(req.features, &mut acc, &mut act);
            results.push(TeacherEval {
                value,
                layers: None,
                domain,
            });
        }
        Ok(results)
    }
}

struct ClassicFp32Teacher {
    net: ClassicFp32Network,
    scratch: RefCell<ClassicTeacherScratch>,
}

#[derive(Default)]
struct ClassicTeacherScratch {
    features_us: Vec<usize>,
    features_them: Vec<usize>,
}

impl ClassicFp32Teacher {
    fn new(net: ClassicFp32Network) -> Self {
        Self {
            net,
            scratch: RefCell::new(ClassicTeacherScratch::default()),
        }
    }
}

impl TeacherNetwork for ClassicFp32Teacher {
    fn kind(&self) -> TeacherKind {
        TeacherKind::ClassicFp32
    }

    fn supports_domain(&self, domain: TeacherValueDomain) -> bool {
        matches!(domain, TeacherValueDomain::WdlLogit)
    }

    fn evaluate_batch<'a>(
        &self,
        batch: &[TeacherBatchRequest<'a>],
        domain: TeacherValueDomain,
        capture_layers: bool,
    ) -> Result<Vec<TeacherEval>, TeacherError> {
        if !batch.is_empty() && !self.supports_domain(domain) {
            return Err(TeacherError::UnsupportedDomain {
                kind: self.kind(),
                domain,
            });
        }

        let mut scratch = self.scratch.borrow_mut();
        let mut results = Vec::with_capacity(batch.len());

        for req in batch {
            scratch.features_us.clear();
            scratch.features_us.extend(req.features.iter().map(|&f| f as usize));
            scratch.features_them.clear();
            let flipped: Vec<usize> =
                scratch.features_us.iter().copied().map(flip_us_them).collect();
            scratch.features_them.extend(flipped);

            let LayerOutputs { ft, h1, h2, output } =
                self.net.forward(&scratch.features_us, &scratch.features_them);
            let layers = if capture_layers {
                Some(TeacherLayers {
                    ft,
                    h1,
                    h2,
                    out: output,
                })
            } else {
                None
            };
            let eval = TeacherEval {
                value: output,
                layers,
                domain: TeacherValueDomain::WdlLogit,
            };
            results.push(eval);
        }

        Ok(results)
    }
}
