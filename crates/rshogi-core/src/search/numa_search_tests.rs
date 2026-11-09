//! `NumaPolicy=auto` の配線を、実機の CPU を 2 つに分けた仮の topology で検証する。

use super::*;
use crate::nnue::network_halfkp::halfkp_loader_fixture;
use crate::nnue::{NNUENetwork, get_network, init_nnue_from_bytes};
use crate::search::numa::{NumaNetwork, NumaTopology, current_affinity, parse_cpulist};

const ARCH: &str = "Features=HalfKP(Friend)[125388->256x2],l2=32,l3=32";
const STACK_SIZE: usize = 64 * 1024 * 1024;

fn halves() -> (Vec<usize>, Vec<usize>) {
    let cpus = current_affinity().unwrap();
    assert!(cpus.len() >= 2, "CPU が 2 つ以上必要");
    let (first, second) = cpus.split_at(cpus.len() / 2);
    (first.to_vec(), second.to_vec())
}

fn fake_two_node_topology() -> NumaTopology {
    let (first, second) = halves();
    NumaTopology::from_nodes(vec![first, second])
}

/// このプロセスの全スレッドの CPU 集合。
fn thread_affinities() -> Vec<Vec<usize>> {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|task| std::fs::read_to_string(task.unwrap().path().join("status")).ok())
        .map(|status| {
            let line = status.lines().find(|l| l.starts_with("Cpus_allowed_list:")).unwrap();
            parse_cpulist(line.trim_start_matches("Cpus_allowed_list:")).unwrap()
        })
        .collect()
}

fn search_nodes(search: &mut Search) -> (u64, Move) {
    let mut pos = Position::new();
    pos.set_hirate();
    let limits = LimitsType {
        depth: 7,
        ..Default::default()
    };
    search.reset_flags();
    let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);
    (result.nodes, result.best_move)
}

fn worker_network(worker: &SearchWorker) -> Option<NumaNetwork> {
    worker.numa_network.clone()
}

fn helper_networks(search: &Search) -> Vec<Option<NumaNetwork>> {
    search
        .thread_pool
        .helper_threads()
        .iter()
        .map(|thread| thread.with_worker(|worker| worker_network(worker)))
        .collect()
}

fn same(a: &Arc<NNUENetwork>, b: &Arc<NNUENetwork>) -> bool {
    Arc::ptr_eq(a, b)
}

fn check_numa_wiring() {
    init_nnue_from_bytes(&halfkp_loader_fixture(256, ARCH)).unwrap();
    let global = get_network().unwrap();

    // none: 状態も写しも作らず、helper も写しを持たない。
    let mut plain = Search::new_with_eval_hash(16, 1);
    plain.set_num_threads(2);
    assert!(plain.set_numa_policy(NumaPolicy::None).is_none());
    assert!(plain.prepare_numa_networks().is_none());
    assert!(plain.numa.is_none());
    assert!(helper_networks(&plain).iter().all(Option::is_none));
    plain.set_num_threads(1);
    let expected = search_nodes(&mut plain);
    assert!(plain.numa.is_none());
    assert!(worker_network(plain.worker.as_ref().unwrap()).is_none());

    // 2 node・2 スレッド: main は node 0、helper は node 1 の写しを読む。
    let all_cpus = current_affinity().unwrap();
    let (_, second_node) = halves();
    let mut numa = Search::new_with_eval_hash(16, 1);
    numa.set_num_threads(2);
    numa.enable_numa(fake_two_node_topology());
    assert!(numa.prepare_numa_networks().is_none(), "固定と写しの作成に成功する");
    assert!(thread_affinities().contains(&second_node), "helper は node 1 に固定される");
    let replicas = numa.numa.as_ref().unwrap().replicas.as_ref().unwrap();
    assert!(same(&replicas.source, &global));
    let main_replica = replicas.for_thread(0).unwrap();
    let helper_replica = helper_networks(&numa)[0].clone().unwrap();
    assert!(!same(&main_replica.local, &global));
    assert!(!same(&helper_replica.local, &global));
    assert!(!same(&main_replica.local, &helper_replica.local), "node ごとに別の写し");

    // go は呼んだスレッドを固定して探索し、抜けるときに元の CPU 集合へ戻す。
    search_nodes(&mut numa);
    assert_eq!(current_affinity().unwrap(), all_cpus);

    // 写しを作れなかったときの退避: helper は作り直さずに全 CPU へ固定し直す。
    let helper_id = numa.thread_pool.helper_threads()[0].id();
    numa.fall_back_to_shared_network();
    assert!(numa.numa.is_none());
    assert_eq!(numa.thread_pool.helper_threads()[0].id(), helper_id);
    assert!(helper_networks(&numa).iter().all(Option::is_none));
    assert!(thread_affinities().iter().all(|cpus| *cpus == all_cpus));
    numa.enable_numa(fake_two_node_topology());
    assert!(numa.prepare_numa_networks().is_none());

    // 1 スレッドの固定深さ探索は none と同じ木になる。
    numa.set_num_threads(1);
    numa.reset_to_fresh_state();
    numa.clear_eval_hash();
    assert!(helper_networks(&numa).is_empty());
    assert_eq!(search_nodes(&mut numa), expected);
    let main = worker_network(numa.worker.as_ref().unwrap()).unwrap();
    assert!(!same(&main.local, &global));

    // 評価関数を読み直すと、作り直す前でも古い写しは選ばれず、作り直すと新しい重みの写しになる。
    init_nnue_from_bytes(&halfkp_loader_fixture(256, ARCH)).unwrap();
    let reloaded = get_network().unwrap();
    assert!(!same(&reloaded, &global));
    let selected = NumaNetwork::select(Some(&main), Some(Arc::clone(&reloaded))).unwrap();
    assert!(same(&selected, &reloaded));
    assert!(numa.prepare_numa_networks().is_none());
    let main = worker_network(numa.worker.as_ref().unwrap()).unwrap();
    assert!(same(&main.source, &reloaded));
    numa.reset_to_fresh_state();
    numa.clear_eval_hash();
    assert_eq!(search_nodes(&mut numa), expected);

    // none に戻すと写しを捨て、helper も写しを持たず、どのスレッドも固定されない。
    numa.set_num_threads(2);
    assert!(numa.set_numa_policy(NumaPolicy::None).is_none());
    assert!(numa.numa.is_none());
    assert!(worker_network(numa.worker.as_ref().unwrap()).is_none());
    assert!(helper_networks(&numa).iter().all(Option::is_none));
    assert!(thread_affinities().iter().all(|cpus| *cpus == all_cpus));
}

#[test]
fn numa_auto_replicates_per_node_and_matches_none() {
    const CHILD: &str = "RSHOGI_NUMA_SEARCH_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(check_numa_wiring)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    // グローバル NNUE のロードと CPU 固定を他の並列テストから隔離する。
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            concat!(module_path!(), "::numa_auto_replicates_per_node_and_matches_none")
                .strip_prefix("rshogi_core::")
                .unwrap(),
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
