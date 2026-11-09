//! topology の検出、スレッドの node への配分と固定、node ごとの写しの作成 (Linux 専用)。

use std::path::Path;
use std::sync::Arc;

use crate::nnue::NNUENetwork;

/// プロセスが使える CPU を node ごとにまとめたもの。CPU が 1 つも無い node は含まない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NumaTopology {
    nodes: Vec<Vec<usize>>,
}

impl NumaTopology {
    /// 実機の topology に依らず多 node の経路を試すための構築口。
    #[cfg(all(test, feature = "halfkx-arch"))]
    pub(crate) fn from_nodes(nodes: Vec<Vec<usize>>) -> Self {
        Self { nodes }
    }
}

/// `0-3,8,10-11` 形式の CPU 一覧を昇順・重複なしの CPU 番号に変換する。
pub(crate) fn parse_cpulist(text: &str) -> Result<Vec<usize>, String> {
    let mut cpus = Vec::new();
    for part in text.trim().split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let parse = |s: &str| {
            s.trim()
                .parse::<usize>()
                .map_err(|e| format!("invalid cpulist entry '{part}': {e}"))
        };
        match part.split_once('-') {
            Some((lo, hi)) => {
                let (lo, hi) = (parse(lo)?, parse(hi)?);
                if lo > hi {
                    return Err(format!("invalid cpulist range '{part}'"));
                }
                cpus.extend(lo..=hi);
            }
            None => cpus.push(parse(part)?),
        }
    }
    cpus.sort_unstable();
    cpus.dedup();
    Ok(cpus)
}

/// `node_root` (通常は `/sys/devices/system/node`) の `node<N>/cpulist` を読み、`allowed` との
/// 交わりをとった topology を返す。node は番号順に並べ、CPU が残らない node は捨てる。
pub(crate) fn read_topology(node_root: &Path, allowed: &[usize]) -> Result<NumaTopology, String> {
    let entries = std::fs::read_dir(node_root)
        .map_err(|e| format!("cannot read {}: {e}", node_root.display()))?;
    let mut numbered = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", node_root.display()))?;
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_prefix("node")) else {
            continue;
        };
        let Ok(id) = id.parse::<usize>() else {
            continue;
        };
        numbered.push((id, entry.path()));
    }
    numbered.sort_unstable_by_key(|(id, _)| *id);

    let mut nodes = Vec::new();
    for (_, path) in numbered {
        let cpulist = path.join("cpulist");
        let text = std::fs::read_to_string(&cpulist)
            .map_err(|e| format!("cannot read {}: {e}", cpulist.display()))?;
        let cpus: Vec<usize> = parse_cpulist(&text)?
            .into_iter()
            .filter(|cpu| allowed.binary_search(cpu).is_ok())
            .collect();
        if !cpus.is_empty() {
            nodes.push(cpus);
        }
    }
    if nodes.is_empty() {
        return Err(format!("no usable NUMA node under {}", node_root.display()));
    }
    Ok(NumaTopology { nodes })
}

/// 各 node の CPU 数に比例して `num_threads` 本を配る (最大剰余法、同点は node 番号の小さい方)。
///
/// 戻り値は node ごとのスレッド数。入力が同じなら結果も同じ。
pub(crate) fn distribute_threads(cpu_counts: &[usize], num_threads: usize) -> Vec<usize> {
    let total: usize = cpu_counts.iter().sum();
    if total == 0 {
        return vec![0; cpu_counts.len()];
    }
    let mut counts: Vec<usize> = cpu_counts.iter().map(|&c| num_threads * c / total).collect();
    let assigned: usize = counts.iter().sum();
    let mut order: Vec<usize> = (0..cpu_counts.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(num_threads * cpu_counts[i] % total), i));
    for &i in order.iter().cycle().take(num_threads - assigned) {
        counts[i] += 1;
    }
    counts
}

/// スレッド番号 (0 = main) ごとの node と、node ごとの CPU 集合。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NumaPlan {
    node_cpus: Vec<Vec<usize>>,
    thread_nodes: Vec<usize>,
}

impl NumaPlan {
    /// topology と探索スレッド数から割り当てを作る。thread 0 から順に node 0 へ詰め、
    /// その node の割り当て数に達したら次の node へ進む。
    pub(crate) fn new(topology: &NumaTopology, num_threads: usize) -> Self {
        let cpu_counts: Vec<usize> = topology.nodes.iter().map(Vec::len).collect();
        let per_node = distribute_threads(&cpu_counts, num_threads);
        let thread_nodes = per_node
            .iter()
            .enumerate()
            .flat_map(|(node, &count)| std::iter::repeat_n(node, count))
            .collect();
        Self {
            node_cpus: topology.nodes.clone(),
            thread_nodes,
        }
    }

    pub(crate) fn node_of_thread(&self, thread_id: usize) -> usize {
        self.thread_nodes[thread_id]
    }

    pub(crate) fn cpus_of_thread(&self, thread_id: usize) -> &[usize] {
        &self.node_cpus[self.node_of_thread(thread_id)]
    }

    pub(crate) fn threads_per_node(&self) -> Vec<usize> {
        let mut counts = vec![0; self.node_cpus.len()];
        for &node in &self.thread_nodes {
            counts[node] += 1;
        }
        counts
    }

    /// 利用者向けの 1 行の要約。
    pub(crate) fn describe(&self) -> String {
        let cpus: Vec<String> = self.node_cpus.iter().map(|c| c.len().to_string()).collect();
        let threads: Vec<String> = self.threads_per_node().iter().map(usize::to_string).collect();
        format!(
            "NumaPolicy=auto: {} nodes, cpus per node {}, threads per node {}",
            self.node_cpus.len(),
            cpus.join("+"),
            threads.join("+")
        )
    }
}

/// 探索スレッドが読む node の写し。`source` は写した元の大域の重みで、大域の重みが
/// 差し替わった後に古い写しを読まないための照合に使う。
#[derive(Clone)]
pub(crate) struct NumaNetwork {
    pub(crate) source: Arc<NNUENetwork>,
    pub(crate) local: Arc<NNUENetwork>,
}

impl NumaNetwork {
    /// `global` がこの写しの元と同じなら写しを、違えば (または写しが無ければ) `global` を返す。
    pub(crate) fn select(
        replica: Option<&Self>,
        global: Option<Arc<NNUENetwork>>,
    ) -> Option<Arc<NNUENetwork>> {
        match (replica, global) {
            (Some(replica), Some(global)) if Arc::ptr_eq(&replica.source, &global) => {
                Some(Arc::clone(&replica.local))
            }
            (_, global) => global,
        }
    }
}

/// node ごとに 1 つずつ作った写しの組。
pub(crate) struct NumaReplicas {
    pub(crate) plan: Arc<NumaPlan>,
    pub(crate) source: Arc<NNUENetwork>,
    per_node: Vec<Option<Arc<NNUENetwork>>>,
}

impl NumaReplicas {
    /// スレッドを 1 本も割り当てていない node を除き、node の CPU に固定した一時スレッドの上で
    /// `source` を深く写す。確保と書き込みをそのスレッドが行うので、写しはその node に載る。
    pub(crate) fn build(plan: Arc<NumaPlan>, source: Arc<NNUENetwork>) -> Result<Self, String> {
        let used = plan.threads_per_node();
        let per_node = std::thread::scope(|scope| {
            let handles: Vec<_> = plan
                .node_cpus
                .iter()
                .zip(&used)
                .map(|(cpus, &threads)| {
                    (threads > 0).then(|| {
                        let source = &source;
                        scope.spawn(move || -> Result<Arc<NNUENetwork>, String> {
                            pin_current_thread(cpus)?;
                            Ok(Arc::new(source.deep_clone()))
                        })
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .map(|h| {
                            h.join().map_err(|_| "NUMA replica thread panicked".to_string())?
                        })
                        .transpose()
                })
                .collect::<Result<Vec<_>, String>>()
        })?;
        Ok(Self {
            plan,
            source,
            per_node,
        })
    }

    /// `thread_id` の探索スレッドが読む写し。
    pub(crate) fn for_thread(&self, thread_id: usize) -> Option<NumaNetwork> {
        let node = self.plan.node_of_thread(thread_id);
        self.per_node[node].as_ref().map(|local| NumaNetwork {
            source: Arc::clone(&self.source),
            local: Arc::clone(local),
        })
    }
}

/// `NumaPolicy=auto` の判定結果。
pub(crate) enum NumaDetection {
    /// 2 つ以上の node にまたがる topology。
    MultiNode(NumaTopology),
    /// 無効にした理由。
    Disabled(String),
}

/// この機械とプロセスの CPU 割り当てから topology を調べる。
pub(crate) fn detect() -> NumaDetection {
    let allowed = match current_affinity() {
        Ok(cpus) => cpus,
        Err(e) => return NumaDetection::Disabled(format!("sched_getaffinity failed: {e}")),
    };
    match read_topology(Path::new("/sys/devices/system/node"), &allowed) {
        Ok(topology) if topology.nodes.len() >= 2 => NumaDetection::MultiNode(topology),
        Ok(_) => NumaDetection::Disabled("usable CPUs are on a single NUMA node".to_string()),
        Err(e) => NumaDetection::Disabled(e),
    }
}

/// 呼び出したスレッドが使える CPU 番号 (昇順)。container の cpuset もここに反映される。
pub(crate) fn current_affinity() -> std::io::Result<Vec<usize>> {
    // SAFETY: cpu_set_t は全ビット 0 が有効な値の POD。
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: pid 0 は呼び出しスレッド。set は size_of 分の書き込み可能な領域で、
    // カーネルはそれを超えて書かない。
    let ret =
        unsafe { libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) };
    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let max = libc::CPU_SETSIZE as usize;
    // SAFETY: cpu < CPU_SETSIZE なので CPU_ISSET は set の範囲内だけを読む。
    Ok((0..max).filter(|&cpu| unsafe { libc::CPU_ISSET(cpu, &set) }).collect())
}

/// 呼び出したスレッドを `cpus` の集合に固定する。
pub(crate) fn pin_current_thread(cpus: &[usize]) -> Result<(), String> {
    // SAFETY: cpu_set_t は全ビット 0 が有効な値の POD。
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for &cpu in cpus {
        if cpu >= libc::CPU_SETSIZE as usize {
            return Err(format!("cpu {cpu} exceeds CPU_SETSIZE"));
        }
        // SAFETY: 直前の検査で cpu < CPU_SETSIZE を確かめており、set の範囲内だけを書く。
        unsafe { libc::CPU_SET(cpu, &mut set) };
    }
    // SAFETY: pid 0 は呼び出しスレッド。set は初期化済みで size_of 分だけ読まれる。
    let ret = unsafe { libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) };
    if ret != 0 {
        return Err(format!("sched_setaffinity failed: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::NumaPolicy;

    #[test]
    fn parse_cpulist_ranges_and_singles() {
        assert_eq!(parse_cpulist("0-3,8,10-11\n").unwrap(), vec![0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(parse_cpulist("5").unwrap(), vec![5]);
        assert_eq!(parse_cpulist("3,1-2,2").unwrap(), vec![1, 2, 3]);
        assert_eq!(parse_cpulist("\n").unwrap(), Vec::<usize>::new());
        assert!(parse_cpulist("4-2").is_err());
        assert!(parse_cpulist("a-b").is_err());
    }

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("rshogi-numa-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_nodes(dir: &Path, nodes: &[(&str, &str)]) {
        for (name, cpulist) in nodes {
            let node = dir.join(name);
            std::fs::create_dir_all(&node).unwrap();
            std::fs::write(node.join("cpulist"), cpulist).unwrap();
        }
    }

    #[test]
    fn read_topology_intersects_affinity_and_drops_empty_nodes() {
        let dir = TempDir::new("intersect");
        write_nodes(
            dir.path(),
            &[
                ("node0", "0-3,8-11\n"),
                ("node1", "4-7,12-15\n"),
                ("node10", "16-19\n"),
            ],
        );
        std::fs::write(dir.path().join("possible"), "0-1\n").unwrap();
        let allowed = [0, 1, 2, 12, 13];
        let topology = read_topology(dir.path(), &allowed).unwrap();
        assert_eq!(topology.nodes, [vec![0, 1, 2], vec![12, 13]]);
    }

    #[test]
    fn read_topology_orders_nodes_numerically() {
        let dir = TempDir::new("order");
        write_nodes(dir.path(), &[("node10", "2\n"), ("node2", "1\n"), ("node0", "0\n")]);
        let topology = read_topology(dir.path(), &[0, 1, 2]).unwrap();
        assert_eq!(topology.nodes, [vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn read_topology_errors_without_nodes() {
        let dir = TempDir::new("empty");
        assert!(read_topology(dir.path(), &[0]).is_err());
        assert!(read_topology(&dir.path().join("missing"), &[0]).is_err());
    }

    #[test]
    fn distribute_threads_is_proportional() {
        assert_eq!(distribute_threads(&[16, 16], 32), vec![16, 16]);
        assert_eq!(distribute_threads(&[16, 16], 7), vec![4, 3]);
        assert_eq!(distribute_threads(&[24, 8], 8), vec![6, 2]);
        assert_eq!(distribute_threads(&[3, 1], 2), vec![2, 0]);
        assert_eq!(distribute_threads(&[16, 16], 1), vec![1, 0]);
        assert_eq!(distribute_threads(&[1, 1], 5), vec![3, 2]);
        for n in 0..70 {
            assert_eq!(distribute_threads(&[10, 7, 3], n).iter().sum::<usize>(), n);
        }
    }

    #[test]
    fn plan_assigns_threads_deterministically() {
        let topology = NumaTopology {
            nodes: vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]],
        };
        let plan = NumaPlan::new(&topology, 5);
        assert_eq!(plan.thread_nodes, vec![0, 0, 0, 1, 1]);
        assert_eq!(plan.cpus_of_thread(0), &[0, 1, 2, 3]);
        assert_eq!(plan.cpus_of_thread(4), &[4, 5, 6, 7]);
        assert_eq!(plan, NumaPlan::new(&topology, 5));
    }

    #[test]
    fn policy_from_usi() {
        assert_eq!(NumaPolicy::from_usi("none"), Some(NumaPolicy::None));
        assert_eq!(NumaPolicy::from_usi("Auto"), Some(NumaPolicy::Auto));
        assert_eq!(NumaPolicy::from_usi("on"), None);
    }
}
