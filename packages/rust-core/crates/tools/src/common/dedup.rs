use crate::common::sfen::normalize_4t;
use crate::common::sfen_ops::canonicalize_4t_with_mirror;
use std::collections::HashSet;

/// In-memory de-duplicator keyed by 4-token SFEN or mirror-canonicalized 4-token SFEN.
pub struct DedupSet {
    set: HashSet<String>,
    canonical_with_mirror: bool,
}

impl DedupSet {
    pub fn new(canonical_with_mirror: bool) -> Self {
        Self {
            set: HashSet::new(),
            canonical_with_mirror,
        }
    }
    pub fn insert(&mut self, sfen: &str) -> bool {
        let key = if self.canonical_with_mirror {
            canonicalize_4t_with_mirror(sfen)
        } else {
            normalize_4t(sfen)
        };
        if let Some(k) = key {
            self.set.insert(k)
        } else {
            false
        }
    }
    pub fn len(&self) -> usize {
        self.set.len()
    }
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_dedup_with_mirror() {
        let a = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/2B4R1/LNSGKGSNL b - 1";
        let b = "lnsgkgsnl/1b5r1/ppppppppp/9/9/9/PPPPPPPPP/1R4B2/LNSGKGSNL b - 1";
        let mut d = DedupSet::new(true);
        assert!(d.insert(a));
        assert!(!d.insert(b));
        assert_eq!(d.len(), 1);
    }
}
