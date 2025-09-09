#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct StatsI64 {
    pub count: usize,
    pub min: i64,
    pub max: i64,
    pub mean: f64,
    pub p50: i64,
    pub p90: i64,
    pub p95: i64,
    pub p99: i64,
}

pub fn quantile_sorted(v: &[i64], q: f64) -> i64 {
    if v.is_empty() {
        return 0;
    }
    let n = v.len();
    let idx = ((n - 1) as f64 * q).round() as usize; // nearest-rank-ish
    v[idx]
}

pub fn compute_stats_exact(values: &[i64]) -> Option<StatsI64> {
    if values.is_empty() {
        return None;
    }
    let count = values.len();
    let mut min_v = i64::MAX;
    let mut max_v = i64::MIN;
    let mut sum: i128 = 0;
    for &x in values {
        min_v = min_v.min(x);
        max_v = max_v.max(x);
        sum += x as i128;
    }
    let mean = sum as f64 / count as f64;
    let mut v = values.to_vec();
    v.sort_unstable();
    let idx = |q: f64| -> usize { (((count - 1) as f64 * q).round() as usize).min(count - 1) };
    Some(StatsI64 {
        count,
        min: min_v,
        max: max_v,
        mean,
        p50: v[idx(0.5)],
        p90: v[idx(0.9)],
        p95: v[idx(0.95)],
        p99: v[idx(0.99)],
    })
}

// P² single-quantile estimator (used via OnlineP2)
#[derive(Clone)]
pub struct P2 {
    p: f64,
    q: [f64; 5],
    n: [i64; 5],
    np: [f64; 5],
    dn: [f64; 5],
    init: Vec<f64>,
}

impl P2 {
    fn new(p: f64) -> Self {
        P2 {
            p,
            q: [0.0; 5],
            n: [0; 5],
            np: [0.0; 5],
            dn: [0.0; 5],
            init: Vec::with_capacity(5),
        }
    }
    fn add(&mut self, x: f64) {
        if self.init.len() < 5 {
            self.init.push(x);
            if self.init.len() == 5 {
                self.init.sort_by(|a, b| a.partial_cmp(b).unwrap());
                for i in 0..5 {
                    self.q[i] = self.init[i];
                    self.n[i] = (i as i64) + 1;
                }
                self.np[0] = 1.0;
                self.np[1] = 1.0 + 2.0 * self.p;
                self.np[2] = 1.0 + 4.0 * self.p;
                self.np[3] = 3.0 + 2.0 * self.p;
                self.np[4] = 5.0;
                self.dn[0] = 0.0;
                self.dn[1] = self.p / 2.0;
                self.dn[2] = self.p;
                self.dn[3] = (1.0 + self.p) / 2.0;
                self.dn[4] = 1.0;
            }
            return;
        }
        let mut k: i32 = -1;
        if x < self.q[0] {
            self.q[0] = x;
            k = 0;
        } else if x >= self.q[4] {
            self.q[4] = x;
            k = 3;
        } else {
            for i in 0..4 {
                if x < self.q[i + 1] {
                    k = i as i32;
                    break;
                }
            }
        }
        for i in 0..5 {
            if (i as i32) > k {
                self.n[i] += 1;
            }
        }
        for i in 0..5 {
            self.np[i] += self.dn[i];
        }
        for i in 1..4 {
            let d = self.np[i] - self.n[i] as f64;
            if (d >= 1.0 && (self.n[i + 1] - self.n[i]) > 1)
                || (d <= -1.0 && (self.n[i] - self.n[i - 1]) > 1)
            {
                let sign = if d > 0.0 { 1.0 } else { -1.0 };
                let n_im1 = self.n[i - 1] as f64;
                let n_i = self.n[i] as f64;
                let n_ip1 = self.n[i + 1] as f64;
                let q_im1 = self.q[i - 1];
                let q_i = self.q[i];
                let q_ip1 = self.q[i + 1];
                let a = (n_i - n_im1 + sign) * (q_ip1 - q_i) / (n_ip1 - n_i);
                let b = (n_ip1 - n_i - sign) * (q_i - q_im1) / (n_i - n_im1);
                let q_par = q_i + sign * (a + b) / (n_ip1 - n_im1);
                if q_par > q_im1 && q_par < q_ip1 {
                    self.q[i] = q_par;
                } else {
                    let j = (i as i32 + sign as i32) as usize;
                    self.q[i] = q_i + sign * (self.q[j] - q_i) / ((self.n[j] as f64) - n_i);
                }
                self.n[i] = (self.n[i] as f64 + sign) as i64;
            }
        }
    }
    pub fn quantile(&self) -> f64 {
        if self.init.len() < 5 {
            if self.init.is_empty() {
                return 0.0;
            }
            let mut v = self.init.clone();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let idx = ((v.len() - 1) as f64 * self.p).round() as usize;
            return v[idx];
        }
        self.q[2]
    }
}

#[derive(Clone)]
pub struct OnlineP2 {
    pub count: usize,
    pub min: i64,
    pub max: i64,
    pub sum: i128,
    q05: P2,
    q50: P2,
    q90: P2,
    q95: P2,
    q99: P2,
}

impl Default for OnlineP2 {
    fn default() -> Self {
        Self::new()
    }
}

impl OnlineP2 {
    pub fn new() -> Self {
        OnlineP2 {
            count: 0,
            min: i64::MAX,
            max: i64::MIN,
            sum: 0,
            q05: P2::new(0.05),
            q50: P2::new(0.5),
            q90: P2::new(0.9),
            q95: P2::new(0.95),
            q99: P2::new(0.99),
        }
    }
    pub fn add(&mut self, v: i64) {
        self.count += 1;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.sum += v as i128;
        let x = v as f64;
        self.q05.add(x);
        self.q50.add(x);
        self.q90.add(x);
        self.q95.add(x);
        self.q99.add(x);
    }
    pub fn stats(&self) -> Option<StatsI64> {
        if self.count == 0 {
            return None;
        }
        Some(StatsI64 {
            count: self.count,
            min: self.min,
            max: self.max,
            mean: self.sum as f64 / self.count as f64,
            p50: self.q50.quantile().round() as i64,
            p90: self.q90.quantile().round() as i64,
            p95: self.q95.quantile().round() as i64,
            p99: self.q99.quantile().round() as i64,
        })
    }
    pub fn q05(&self) -> i64 {
        self.q05.quantile().round() as i64
    }
}

#[derive(Default, Clone)]
pub struct TDigest {
    centroids: Vec<(f64, f64)>,
    buf: Vec<f64>,
    compression: f64,
}
impl TDigest {
    pub fn new(compression: usize) -> Self {
        TDigest {
            centroids: Vec::new(),
            buf: Vec::new(),
            compression: (compression as f64).clamp(20.0, 1000.0),
        }
    }
    // Flush buffered samples into centroids (single compress pass)
    pub fn flush(&mut self) {
        self.compress();
    }
    pub fn add(&mut self, x: f64) {
        self.buf.push(x);
        if self.buf.len() > 2048 {
            self.compress();
        }
    }
    #[inline]
    fn k(&self, q: f64) -> f64 {
        (self.compression / std::f64::consts::PI) * (2.0 * q - 1.0).asin()
    }
    fn compress(&mut self) {
        if self.buf.is_empty() && self.centroids.len() <= 1 {
            return;
        }
        let mut all: Vec<(f64, f64)> = Vec::with_capacity(self.centroids.len() + self.buf.len());
        all.append(&mut self.centroids);
        for &x in &self.buf {
            all.push((x, 1.0));
        }
        self.buf.clear();
        if all.is_empty() {
            return;
        }
        all.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let total: f64 = all.iter().map(|c| c.1).sum();
        let mut newc: Vec<(f64, f64)> = Vec::with_capacity(all.len());
        let mut cur = all[0];
        let mut cum_w = 0.0f64;
        for &next in all.iter().skip(1) {
            let projected_w = cur.1 + next.1;
            let q0 = (cum_w) / total;
            let q2 = (cum_w + projected_w) / total;
            let k0 = self.k(q0);
            let k2 = self.k(q2);
            if (k2 - k0) <= 1.0 {
                let w = cur.1 + next.1;
                let m = (cur.0 * cur.1 + next.0 * next.1) / w;
                cur = (m, w);
            } else {
                cum_w += cur.1;
                newc.push(cur);
                cur = next;
            }
        }
        newc.push(cur);
        self.centroids = newc;
    }
    pub fn quantile(&mut self, q: f64) -> f64 {
        if !self.buf.is_empty() {
            self.compress();
        }
        if self.centroids.is_empty() {
            return 0.0;
        }
        let total: f64 = self.centroids.iter().map(|c| c.1).sum();
        if total == 0.0 {
            return 0.0;
        }
        let target = q.clamp(0.0, 1.0) * total;
        let mut cum = 0.0f64;
        for i in 0..self.centroids.len() {
            let (m_i, w_i) = self.centroids[i];
            let left = cum + 0.5 * w_i;
            if target < left {
                if i == 0 {
                    return m_i;
                }
                let (m_prev, w_prev) = self.centroids[i - 1];
                let prev_right = cum - 0.5 * w_prev;
                let span = left - prev_right;
                if span <= 0.0 {
                    return m_prev;
                }
                let t = (target - prev_right) / span;
                return m_prev + (m_i - m_prev) * t;
            }
            let right = cum + w_i;
            if target <= right {
                return m_i;
            }
            cum += w_i;
        }
        self.centroids.last().unwrap().0
    }
}

#[derive(Default, Clone)]
pub struct OnlineTDigest {
    pub count: usize,
    pub min: i64,
    pub max: i64,
    pub sum: i128,
    pub td: TDigest,
}
impl OnlineTDigest {
    pub fn new() -> Self {
        OnlineTDigest {
            count: 0,
            min: i64::MAX,
            max: i64::MIN,
            sum: 0,
            td: TDigest::new(200),
        }
    }
    pub fn add(&mut self, v: i64) {
        self.count += 1;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.sum += v as i128;
        self.td.add(v as f64);
    }
    // Ensure internal buffer is compressed so reads won't need cloning
    pub fn flush(&mut self) {
        self.td.flush();
    }
    pub fn stats(&mut self) -> Option<StatsI64> {
        if self.count == 0 {
            return None;
        }
        Some(StatsI64 {
            count: self.count,
            min: self.min,
            max: self.max,
            mean: self.sum as f64 / self.count as f64,
            p50: self.td.quantile(0.5).round() as i64,
            p90: self.td.quantile(0.9).round() as i64,
            p95: self.td.quantile(0.95).round() as i64,
            p99: self.td.quantile(0.99).round() as i64,
        })
    }
    pub fn q(&mut self, q: f64) -> i64 {
        self.td.quantile(q).round() as i64
    }
}

// ------------------------
// Additional metrics utils
// ------------------------

/// Weighted ROC-AUC for binary labels (labels >= 0.5 treated as positive).
/// Returns None if there are no positive or no negative examples.
pub fn roc_auc_weighted(scores: &[f32], labels: &[f32], weights: &[f32]) -> Option<f64> {
    use std::cmp::Ordering;
    let n = scores.len();
    if n == 0 || labels.len() != n || weights.len() != n {
        return None;
    }
    let mut items: Vec<(f32, f32, f32)> =
        (0..n).map(|i| (scores[i], labels[i], weights[i])).collect();
    items.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));

    let mut wpos = 0.0f64;
    let mut wneg = 0.0f64;
    for &(_, y, w) in &items {
        if y >= 0.5 {
            wpos += w as f64;
        } else {
            wneg += w as f64;
        }
    }
    if wpos == 0.0 || wneg == 0.0 {
        return None;
    }

    let mut auc_num = 0.0f64;
    let mut neg_cum = 0.0f64;
    let mut i = 0;
    while i < n {
        let s = items[i].0;
        let mut j = i;
        let mut pos_sum = 0.0f64;
        let mut neg_sum = 0.0f64;
        while j < n && items[j].0 == s {
            if items[j].1 >= 0.5 {
                pos_sum += items[j].2 as f64;
            } else {
                neg_sum += items[j].2 as f64;
            }
            j += 1;
        }
        auc_num += pos_sum * neg_cum + 0.5 * pos_sum * neg_sum;
        neg_cum += neg_sum;
        i = j;
    }
    Some(auc_num / (wpos * wneg))
}

/// Binary classification metrics for WDL probability predictions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BinaryMetrics {
    pub logloss: f64,
    pub brier: f64,
    pub accuracy: f64,
}

pub fn binary_metrics(probs: &[f32], labels: &[f32], weights: &[f32]) -> Option<BinaryMetrics> {
    let n = probs.len();
    if n == 0 || labels.len() != n || weights.len() != n {
        return None;
    }
    let mut wsum = 0.0f64;
    let mut logloss = 0.0f64;
    let mut brier = 0.0f64;
    let mut correct = 0.0f64;
    for i in 0..n {
        let p = probs[i].clamp(1e-7, 1.0 - 1e-7) as f64;
        let y = if labels[i] >= 0.5 { 1.0 } else { 0.0 };
        let w = weights[i] as f64;
        logloss += -w * (y * p.ln() + (1.0 - y) * (1.0 - p).ln());
        brier += w * (p - y) * (p - y);
        correct += w * if (p >= 0.5) as i32 as f64 == y {
            1.0
        } else {
            0.0
        };
        wsum += w;
    }
    if wsum == 0.0 {
        return None;
    }
    Some(BinaryMetrics {
        logloss: logloss / wsum,
        brier: brier / wsum,
        accuracy: correct / wsum,
    })
}

/// Regression metrics (CP prediction) with weights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegMetrics {
    pub mse: f64,
    pub mae: f64,
}

pub fn regression_metrics(pred: &[f32], label: &[f32], weights: &[f32]) -> Option<RegMetrics> {
    let n = pred.len();
    if n == 0 || label.len() != n || weights.len() != n {
        return None;
    }
    let mut wsum = 0.0f64;
    let mut mse = 0.0f64;
    let mut mae = 0.0f64;
    for i in 0..n {
        let e = (pred[i] - label[i]) as f64;
        let w = weights[i] as f64;
        mse += w * e * e;
        mae += w * e.abs();
        wsum += w;
    }
    if wsum == 0.0 {
        return None;
    }
    Some(RegMetrics {
        mse: mse / wsum,
        mae: mae / wsum,
    })
}

/// Calibration bin summary for cp↔wdl calibration plotting.
#[derive(Clone, Debug)]
pub struct CalibBin {
    pub left: i32,
    pub right: i32,
    pub center: f32,
    pub count: usize,
    pub weighted_count: f64,
    pub mean_pred: f64,
    pub mean_label: f64,
}

/// Build equal-width calibration bins across [-clip, clip].
pub fn calibration_bins(
    cps: &[i32],
    probs: &[f32],
    labels: &[f32],
    weights: &[f32],
    clip: i32,
    nbins: usize,
) -> Vec<CalibBin> {
    let n = cps.len().min(probs.len()).min(labels.len()).min(weights.len());
    let nb = nbins.max(1);
    let width = (2 * clip).max(1) as f32 / nb as f32;
    let mut bins: Vec<CalibBin> = (0..nb)
        .map(|b| {
            let l = (-clip as f32 + b as f32 * width).round() as i32;
            let r = (-clip as f32 + (b as f32 + 1.0) * width).round() as i32;
            let c = (l + r) as f32 / 2.0;
            CalibBin {
                left: l,
                right: r,
                center: c,
                count: 0,
                weighted_count: 0.0,
                mean_pred: 0.0,
                mean_label: 0.0,
            }
        })
        .collect();
    if n == 0 {
        return bins;
    }
    for i in 0..n {
        let cp = cps[i].clamp(-clip, clip);
        let idx = (((cp + clip) as f32) / width).floor() as usize;
        let idx = idx.min(nb - 1);
        let w = weights[i] as f64;
        bins[idx].count += 1;
        bins[idx].weighted_count += w;
        bins[idx].mean_pred += w * (probs[i] as f64);
        // Use soft mean (average of provided labels) for calibration target
        bins[idx].mean_label += w * (labels[i] as f64);
    }
    for b in &mut bins {
        if b.weighted_count > 0.0 {
            b.mean_pred /= b.weighted_count;
            b.mean_label /= b.weighted_count;
        }
    }
    bins
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p2_monotonic_bounds() {
        let mut o = OnlineP2::new();
        for i in 0..1000 {
            o.add(i);
        }
        let s = o.stats().unwrap();
        assert!(s.p50 >= s.min && s.p50 <= s.max);
        assert!(s.p95 >= s.p50);
    }

    #[test]
    fn tdigest_basic_bounds() {
        let mut o = OnlineTDigest::new();
        for i in 0..1000 {
            o.add(i);
        }
        let s = o.stats().unwrap();
        assert!(s.p50 >= s.min && s.p50 <= s.max);
        assert!(s.p99 >= s.p95);
    }
}
