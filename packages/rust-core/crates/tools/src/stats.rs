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
    Some(StatsI64 {
        count,
        min: min_v,
        max: max_v,
        mean,
        p50: quantile_sorted(&v, 0.5),
        p90: quantile_sorted(&v, 0.9),
        p95: quantile_sorted(&v, 0.95),
        p99: quantile_sorted(&v, 0.99),
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
        let mut s = o.stats().unwrap();
        assert!(s.p50 >= s.min && s.p50 <= s.max);
        assert!(s.p99 >= s.p95);
    }
}
