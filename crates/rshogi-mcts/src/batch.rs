use std::mem::size_of;

use rshogi_dl::{InputBatchBuffer, InputFormat};

use super::*;

#[derive(Clone, Copy)]
enum LeafValue {
    Neural(usize),
    Known(f32),
    Terminal(Outcome, Move),
}

struct Pending {
    node: usize,
    depth: usize,
    legal_len: usize,
    value: LeafValue,
}

pub(super) struct Workspace {
    format: InputFormat,
    capacity: usize,
    depth: usize,
    pending: Vec<Pending>,
    paths: Vec<usize>,
    moves: Vec<Move>,
    labels: Vec<usize>,
    inputs: InputBatchBuffer,
    logits: Vec<f32>,
    values: Vec<f32>,
    node_inflight: Vec<u32>,
    edge_inflight: Vec<u32>,
    node_blocked: Vec<bool>,
    blocked: Vec<usize>,
    reserved_edges: usize,
    bytes: usize,
}

fn elements(rows: usize, columns: usize) -> Result<usize> {
    rows.checked_mul(columns)
        .ok_or_else(|| anyhow::anyhow!("batch workspace size overflow"))
}

fn filled<T: Clone>(len: usize, value: T) -> Result<Vec<T>> {
    ensure!(
        len.checked_mul(size_of::<T>())
            .is_some_and(|bytes| bytes <= isize::MAX as usize),
        "batch workspace size overflow"
    );
    let mut values = Vec::new();
    values.try_reserve_exact(len)?;
    values.resize(len, value);
    Ok(values)
}

#[cfg(feature = "lazy-batch-metadata")]
fn growth_target(required: usize, limit: usize) -> Result<usize> {
    ensure!(required > 0 && required <= limit, "batch metadata exceeds arena limit");
    Ok(required.checked_next_power_of_two().unwrap_or(limit).min(limit))
}

#[cfg(all(test, feature = "lazy-batch-metadata"))]
mod growth_fault {
    use std::cell::Cell;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum Point {
        NodeInflight,
        NodeBlocked,
        EdgeInflight,
    }

    thread_local! {
        static POINT: Cell<Option<Point>> = const { Cell::new(None) };
    }

    pub(super) struct Guard;

    impl Guard {
        pub(super) fn arm(point: Point) -> Self {
            POINT.with(|slot| {
                assert!(slot.get().is_none());
                slot.set(Some(point));
            });
            Self
        }

        pub(super) fn is_armed(&self) -> bool {
            POINT.with(|slot| slot.get().is_some())
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            POINT.with(|slot| slot.set(None));
        }
    }

    pub(super) fn check(point: Point) -> Result<(), anyhow::Error> {
        anyhow::ensure!(POINT.with(|slot| slot.get()) != Some(point), "metadata allocation failed");
        Ok(())
    }
}

impl Workspace {
    pub(super) fn new(format: InputFormat, capacity: usize, search: &Search) -> Result<Self> {
        ensure!((2..=1024).contains(&capacity), "batch capacity must be 2..=1024");
        let depth = search.config.max_depth;
        let mut pending = Vec::new();
        pending.try_reserve_exact(capacity)?;
        let mut blocked = Vec::new();
        blocked.try_reserve_exact(elements(capacity, depth + 1)?)?;
        #[cfg(feature = "lazy-batch-metadata")]
        let (node_count, edge_count) = (search.nodes.len(), search.edges.len());
        #[cfg(not(feature = "lazy-batch-metadata"))]
        let (node_count, edge_count) = (search.node_limit, search.edge_limit);
        let mut workspace = Self {
            format,
            capacity,
            depth,
            pending,
            paths: filled(elements(capacity, depth)?, 0)?,
            moves: filled(elements(capacity, MAX_MOVES)?, Move::NONE)?,
            labels: filled(elements(capacity, MAX_MOVES)?, 0)?,
            inputs: InputBatchBuffer::new(format, capacity)?,
            logits: filled(elements(capacity, MAX_MOVE_LABEL_NUM)?, 0.0)?,
            values: filled(capacity, 0.0)?,
            node_inflight: filled(node_count, 0)?,
            edge_inflight: filled(edge_count, 0)?,
            node_blocked: filled(node_count, false)?,
            blocked,
            reserved_edges: 0,
            bytes: 0,
        };
        workspace.bytes = workspace.measure_bytes()?;
        Ok(workspace)
    }

    #[cfg(feature = "lazy-batch-metadata")]
    fn ensure_nodes(&mut self, required: usize, limit: usize) -> Result<()> {
        if required <= self.node_inflight.len() {
            return Ok(());
        }
        let target = growth_target(required, limit)?;
        if self.node_inflight.capacity() < target {
            #[cfg(test)]
            growth_fault::check(growth_fault::Point::NodeInflight)?;
            self.node_inflight.try_reserve_exact(target - self.node_inflight.len())?;
        }
        if self.node_blocked.capacity() < target {
            #[cfg(test)]
            growth_fault::check(growth_fault::Point::NodeBlocked)?;
            self.node_blocked.try_reserve_exact(target - self.node_blocked.len())?;
        }
        self.node_inflight.resize(target, 0);
        self.node_blocked.resize(target, false);
        self.bytes = self.measure_bytes()?;
        Ok(())
    }

    #[cfg(feature = "lazy-batch-metadata")]
    fn ensure_edges(&mut self, required: usize, limit: usize) -> Result<()> {
        if required <= self.edge_inflight.len() {
            return Ok(());
        }
        let target = growth_target(required, limit)?;
        if self.edge_inflight.capacity() < target {
            #[cfg(test)]
            growth_fault::check(growth_fault::Point::EdgeInflight)?;
            self.edge_inflight.try_reserve_exact(target - self.edge_inflight.len())?;
        }
        self.edge_inflight.resize(target, 0);
        self.bytes = self.measure_bytes()?;
        Ok(())
    }

    pub(super) fn matches(&self, format: InputFormat, capacity: usize) -> bool {
        self.format == format && self.capacity == capacity
    }

    fn measure_bytes(&self) -> Result<usize> {
        let mut bytes = size_of::<Self>() - size_of::<InputBatchBuffer>();
        for (capacity, element) in [
            (self.pending.capacity(), size_of::<Pending>()),
            (self.paths.capacity(), size_of::<usize>()),
            (self.labels.capacity(), size_of::<usize>()),
            (self.blocked.capacity(), size_of::<usize>()),
            (self.moves.capacity(), size_of::<Move>()),
            (self.logits.capacity(), size_of::<f32>()),
            (self.values.capacity(), size_of::<f32>()),
            (self.node_inflight.capacity(), size_of::<u32>()),
            (self.edge_inflight.capacity(), size_of::<u32>()),
            (self.node_blocked.capacity(), size_of::<bool>()),
            (self.inputs.allocated_bytes(), 1),
        ] {
            bytes = bytes
                .checked_add(elements(capacity, element)?)
                .ok_or_else(|| anyhow::anyhow!("batch workspace size overflow"))?;
        }
        ensure!(bytes <= isize::MAX as usize, "batch workspace size overflow");
        Ok(bytes)
    }

    pub(super) fn allocated_bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub(super) fn is_idle(&self) -> bool {
        self.pending.is_empty()
            && self.inputs.is_empty()
            && self.reserved_edges == 0
            && self.node_inflight.iter().all(|&count| count == 0)
            && self.edge_inflight.iter().all(|&count| count == 0)
            && self.node_blocked.iter().all(|&blocked| !blocked)
            && self.blocked.is_empty()
    }

    fn reserve(&mut self, search: &Search, node: usize, legal: &[Move], value: LeafValue) {
        let row = self.pending.len();
        self.paths[row * self.depth..row * self.depth + search.path.len()]
            .copy_from_slice(&search.path);
        self.moves[row * MAX_MOVES..row * MAX_MOVES + legal.len()].copy_from_slice(legal);
        self.node_inflight[0] += 1;
        for &index in &search.path {
            self.edge_inflight[index] += 1;
            self.node_inflight[search.edges[index].child.expect("path has a child")] += 1;
        }
        self.reserved_edges += legal.len();
        self.pending.push(Pending {
            node,
            depth: search.path.len(),
            legal_len: legal.len(),
            value,
        });
    }

    fn release(&mut self, search: &Search, row: usize) {
        self.node_inflight[0] -= 1;
        let pending = &self.pending[row];
        for &index in &self.paths[row * self.depth..row * self.depth + pending.depth] {
            self.edge_inflight[index] -= 1;
            self.node_inflight[search.edges[index].child.expect("path has a child")] -= 1;
        }
        self.reserved_edges -= pending.legal_len;
    }

    fn clear(&mut self) {
        for index in self.blocked.drain(..) {
            self.node_blocked[index] = false;
        }
        self.pending.clear();
        self.inputs.clear();
    }

    fn cancel(&mut self, search: &Search) {
        for row in 0..self.pending.len() {
            self.release(search, row);
        }
        self.clear();
    }

    fn block(&mut self, index: usize) {
        if !self.node_blocked[index] {
            self.node_blocked[index] = true;
            self.blocked.push(index);
        }
    }
}

pub(super) struct Control<'a> {
    pub limits: Limits,
    pub stop: &'a AtomicBool,
    pub start: Instant,
    pub history_len: usize,
}

#[cfg(test)]
mod expiry_poll {
    use std::cell::Cell;

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(super) enum Cause {
        Stop,
        Deadline,
    }

    thread_local! {
        static STATE: Cell<Option<(usize, usize, Cause)>> = const { Cell::new(None) };
    }

    pub(super) struct Guard;

    impl Guard {
        pub(super) fn arm(target: usize, cause: Cause) -> Self {
            STATE.with(|state| {
                assert!(state.get().is_none());
                state.set(Some((0, target, cause)));
            });
            Self
        }

        pub(super) fn polls(&self) -> usize {
            STATE.with(|state| state.get().expect("poll hook is armed").0)
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            STATE.with(|state| state.set(None));
        }
    }

    pub(super) fn poll() -> Option<Cause> {
        STATE.with(|state| {
            let (count, target, cause) = state.get()?;
            let count = count + 1;
            state.set(Some((count, target, cause)));
            (count >= target).then_some(cause)
        })
    }
}

impl Control<'_> {
    fn expired(&self) -> bool {
        #[cfg(test)]
        let injected = expiry_poll::poll();
        #[cfg(test)]
        if injected == Some(expiry_poll::Cause::Stop) {
            self.stop.store(true, Ordering::Relaxed);
        }
        self.stop.load(Ordering::Relaxed)
            || self.limits.time.is_some_and(|limit| {
                #[cfg(test)]
                if injected == Some(expiry_poll::Cause::Deadline) {
                    return true;
                }
                self.start.elapsed() >= limit
            })
    }
}

enum Collected {
    Neural,
    Known,
    Blocked,
    MemoryFull,
    Stopped,
}

fn select(search: &Search, workspace: &Workspace, index: usize) -> Option<usize> {
    let node = &search.nodes[index];
    let scale = search.config.cpuct
        * ((node.visits + u64::from(workspace.node_inflight[index])).max(1) as f64).sqrt() as f32;
    let mut selected = None;
    let mut maximum = f32::NEG_INFINITY;
    for index in node.first_edge..node.first_edge + node.edge_count {
        let edge = &search.edges[index];
        if let Some(child) = edge.child {
            let child_node = &search.nodes[child];
            if workspace.node_blocked[child]
                || (!child_node.expanded
                    && child_node.outcome.is_none()
                    && workspace.node_inflight[child] > 0)
            {
                continue;
            }
            if child_node.outcome == Some(Outcome::Loss) {
                return Some(index);
            }
            if child_node.outcome == Some(Outcome::Win) {
                continue;
            }
        }
        let q = if edge.visits == 0 {
            0.5
        } else {
            (edge.value_sum / edge.visits as f64) as f32
        };
        let score = q + scale * edge.prior
            / (1.0 + edge.visits as f32 + workspace.edge_inflight[index] as f32);
        if score > maximum {
            maximum = score;
            selected = Some(index);
        }
    }
    selected
}

fn collect(
    search: &mut Search,
    workspace: &mut Workspace,
    pos: &mut Position,
    history: &mut History,
    control: &Control<'_>,
) -> Result<Collected> {
    let mut index = 0;
    loop {
        if control.expired() {
            return Ok(Collected::Stopped);
        }
        if let Some(outcome) = search.nodes[index].outcome {
            workspace.reserve(search, index, &[], LeafValue::Known(outcome.value()));
            return Ok(Collected::Known);
        }
        if !search.nodes[index].expanded {
            if workspace.node_inflight[index] > 0 {
                return Ok(Collected::Blocked);
            }
            let mut legal = MoveList::new();
            generate_legal_all(pos, &mut legal);
            search.set_terminal(index, pos, history, legal.is_empty());
            if let Some(outcome) = search.nodes[index].outcome {
                let declaration = search.nodes[index].declaration_move;
                search.nodes[index].outcome = None;
                search.nodes[index].declaration_move = Move::NONE;
                workspace.reserve(search, index, &[], LeafValue::Terminal(outcome, declaration));
                return Ok(Collected::Known);
            }
            let future_edges = search
                .edges
                .len()
                .checked_add(workspace.reserved_edges)
                .and_then(|len| len.checked_add(legal.len()))
                .ok_or_else(|| anyhow::anyhow!("edge reservation overflow"))?;
            if future_edges > search.edge_limit {
                return Ok(Collected::MemoryFull);
            }
            #[cfg(feature = "lazy-batch-metadata")]
            workspace.ensure_edges(future_edges, search.edge_limit)?;
            let row = workspace.pending.len();
            for (column, &mv) in legal.iter().enumerate() {
                workspace.labels[row * MAX_MOVES + column] =
                    make_move_label(mv, pos.side_to_move());
            }
            let input = workspace.inputs.len();
            workspace.inputs.push(pos)?;
            workspace.reserve(search, index, legal.as_slice(), LeafValue::Neural(input));
            return Ok(Collected::Neural);
        }
        if search.path.len() == search.config.max_depth {
            workspace.reserve(
                search,
                index,
                &[],
                LeafValue::Known(search.nodes[index].static_value),
            );
            return Ok(Collected::Known);
        }
        let Some(edge_index) = select(search, workspace, index) else {
            workspace.block(index);
            let Some(edge_index) = search.path.pop() else {
                return Ok(Collected::Blocked);
            };
            pos.undo_move(search.edges[edge_index].mv);
            history.truncate(history.len() - 1);
            index = search
                .path
                .last()
                .map_or(0, |&edge| search.edges[edge].child.expect("path has a child"));
            continue;
        };
        if search.edges[edge_index].child.is_none() {
            if search.nodes.len() == search.node_limit {
                return Ok(Collected::MemoryFull);
            }
            #[cfg(feature = "lazy-batch-metadata")]
            workspace.ensure_nodes(
                search
                    .nodes
                    .len()
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("node reservation overflow"))?,
                search.node_limit,
            )?;
            search.edges[edge_index].child = Some(search.nodes.len());
            search.nodes.push(Node::new());
        }
        let mv = search.edges[edge_index].mv;
        pos.do_move(mv, pos.gives_check(mv));
        history.push(pos);
        search.path.push(edge_index);
        index = search.edges[edge_index].child.expect("child was initialized");
    }
}

fn commit(search: &mut Search, workspace: &mut Workspace) {
    for row in 0..workspace.pending.len() {
        workspace.release(search, row);
        let pending = &workspace.pending[row];
        let index = pending.node;
        let value = match pending.value {
            LeafValue::Known(value) => value,
            LeafValue::Terminal(outcome, declaration) => {
                search.nodes[index].outcome = Some(outcome);
                search.nodes[index].declaration_move = declaration;
                outcome.value()
            }
            LeafValue::Neural(input) => {
                let mut priors = [0.0f32; MAX_MOVES];
                let priors = &mut priors[..pending.legal_len];
                for (column, prior) in priors.iter_mut().enumerate() {
                    *prior = workspace.logits
                        [input * MAX_MOVE_LABEL_NUM + workspace.labels[row * MAX_MOVES + column]];
                }
                let max = priors.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let mut sum = 0.0;
                for prior in priors.iter_mut() {
                    *prior = ((*prior - max) / search.config.temperature).exp();
                    sum += *prior;
                }
                let first = search.edges.len();
                for (column, &prior) in priors.iter().enumerate() {
                    search.edges.push(Edge {
                        mv: workspace.moves[row * MAX_MOVES + column],
                        prior: prior / sum,
                        child: None,
                        visits: 0,
                        value_sum: 0.0,
                    });
                }
                let node = &mut search.nodes[index];
                node.first_edge = first;
                node.edge_count = pending.legal_len;
                node.static_value = workspace.values[input];
                node.expanded = true;
                workspace.values[input]
            }
        };
        search.nodes[index].visits += 1;
        search.nodes[index].value_sum += f64::from(value);
        let mut value = value;
        let path = &workspace.paths[row * workspace.depth..row * workspace.depth + pending.depth];
        for path_index in (0..path.len()).rev() {
            value = 1.0 - value;
            let edge_index = path[path_index];
            search.edges[edge_index].visits += 1;
            search.edges[edge_index].value_sum += f64::from(value);
            let parent = if path_index == 0 {
                0
            } else {
                search.edges[path[path_index - 1]].child.expect("path has a child")
            };
            search.nodes[parent].visits += 1;
            search.nodes[parent].value_sum += f64::from(value);
            search.propagate_outcome(parent);
        }
    }
    workspace.clear();
}

pub(super) fn run(
    search: &mut Search,
    pos: &mut Position,
    history: &mut History,
    evaluator: &mut impl Evaluator,
    workspace: &mut Workspace,
    control: Control<'_>,
) -> Result<bool> {
    let result = (|| {
        let mut memory_full = false;
        while search.nodes[0].visits < control.limits.visits
            && search.nodes[0].outcome.is_none()
            && !control.expired()
        {
            while workspace.pending.len() < workspace.capacity
                && (workspace.pending.len() as u64)
                    < control.limits.visits.saturating_sub(search.nodes[0].visits)
            {
                search.path.clear();
                let collected = collect(search, workspace, pos, history, &control);
                for &edge in search.path.iter().rev() {
                    pos.undo_move(search.edges[edge].mv);
                }
                history.truncate(control.history_len);
                search.path.clear();
                match collected? {
                    Collected::Neural => {}
                    Collected::MemoryFull => {
                        memory_full = true;
                        break;
                    }
                    Collected::Known | Collected::Blocked | Collected::Stopped => break,
                }
                if !search.nodes[0].expanded {
                    break;
                }
            }
            if workspace.pending.is_empty() || control.expired() {
                break;
            }
            if !workspace.inputs.is_empty() {
                let count = workspace.inputs.len();
                let values = &mut workspace.values[..count];
                let logits = &mut workspace.logits[..count * MAX_MOVE_LABEL_NUM];
                evaluator.evaluate_encoded_batch(workspace.inputs.view(), values, logits)?;
                ensure!(
                    values.iter().all(|value| value.is_finite() && (0.0..=1.0).contains(value)),
                    "invalid batch value probability"
                );
                ensure!(
                    logits.iter().all(|value| value.is_finite()),
                    "non-finite batch policy logit"
                );
                search.evaluations += count as u64;
            }
            commit(search, workspace);
            if memory_full {
                break;
            }
        }
        Ok(memory_full)
    })();
    workspace.cancel(search);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        scalar_calls: usize,
        batches: Vec<Vec<[u8; rshogi_dl::tokens::TOKEN_COUNT]>>,
    }

    impl Evaluator for Recorder {
        fn input_format(&self) -> Option<InputFormat> {
            Some(InputFormat::StmTokensV1)
        }

        fn encoded_batch_capacity(&self) -> Option<usize> {
            Some(4)
        }

        fn evaluate(&mut self, _: &Position, _: &mut [f32]) -> Result<f32> {
            self.scalar_calls += 1;
            anyhow::bail!("must not evaluate scalar")
        }

        fn evaluate_encoded_batch(
            &mut self,
            input: rshogi_dl::EncodedBatch<'_>,
            values: &mut [f32],
            logits: &mut [f32],
        ) -> Result<()> {
            self.batches.push(
                (0..input.len()).map(|row| *input.row(row).unwrap().tokens().unwrap()).collect(),
            );
            values.fill(0.3);
            logits.fill(0.0);
            Ok(())
        }
    }

    fn resolved(pos: &Position, usi: &str) -> Move {
        let mv = pos.to_move(Move::from_usi(usi).unwrap()).unwrap();
        let mut legal = MoveList::new();
        generate_legal_all(pos, &mut legal);
        assert!(legal.contains(mv));
        mv
    }

    fn edge(mv: Move, prior: f32, child: usize) -> Edge {
        Edge {
            mv,
            prior,
            child: Some(child),
            visits: 0,
            value_sum: 0.0,
        }
    }

    fn expand(search: &mut Search, index: usize, first: usize, count: usize) {
        let node = &mut search.nodes[index];
        node.expanded = true;
        node.first_edge = first;
        node.edge_count = count;
    }

    fn restore_root(search: &mut Search, pos: &mut Position, history: &mut History, len: usize) {
        for &index in search.path.iter().rev() {
            pos.undo_move(search.edges[index].mv);
        }
        history.truncate(len);
        search.path.clear();
    }

    #[test]
    fn expiry_poll_hook_clears_after_normal_drop_and_unwind() {
        {
            let guard = expiry_poll::Guard::arm(1, expiry_poll::Cause::Stop);
            assert!(matches!(expiry_poll::poll(), Some(expiry_poll::Cause::Stop)));
            assert_eq!(guard.polls(), 1);
        }
        assert!(expiry_poll::poll().is_none());
        let result = std::panic::catch_unwind(|| {
            let guard = expiry_poll::Guard::arm(1, expiry_poll::Cause::Deadline);
            assert!(matches!(expiry_poll::poll(), Some(expiry_poll::Cause::Deadline)));
            assert_eq!(guard.polls(), 1);
            panic!("exercise poll hook cleanup");
        });
        assert!(result.is_err());
        assert!(expiry_poll::poll().is_none());
    }

    #[test]
    fn expiry_during_collection_cancels_pending_at_the_dispatch_guard() -> Result<()> {
        for cause in [expiry_poll::Cause::Stop, expiry_poll::Cause::Deadline] {
            let mut search = Search::new(Config {
                batch_size: 4,
                tree_memory_mb: 1,
                ..Config::default()
            })?;
            search.nodes.resize_with(3, Node::new);
            let mut pos = Position::new();
            pos.set_hirate();
            let original = pos.to_sfen();
            search.edges.push(edge(resolved(&pos, "7g7f"), 0.5, 1));
            search.edges.push(edge(resolved(&pos, "2g2f"), 0.5, 2));
            expand(&mut search, 0, 0, 2);
            let mut workspace = Workspace::new(InputFormat::StmTokensV1, 4, &search)?;
            let mut history = History::new(&pos);
            let stop = AtomicBool::new(false);
            let control = Control {
                limits: Limits {
                    visits: 4,
                    time: (cause == expiry_poll::Cause::Deadline).then_some(Duration::MAX),
                },
                stop: &stop,
                start: Instant::now(),
                history_len: history.len(),
            };
            // outer + 最初の root/leaf + 次の root を通し、次の leaf で期限を進める。
            let hook = expiry_poll::Guard::arm(5, cause);
            let mut evaluator = Recorder::default();
            assert!(!run(
                &mut search,
                &mut pos,
                &mut history,
                &mut evaluator,
                &mut workspace,
                control
            )?);
            assert_eq!(hook.polls(), 6);
            assert_eq!(workspace.paths[0], 0);
            assert_ne!(workspace.moves[0], Move::NONE);
            assert!(evaluator.batches.is_empty());
            assert_eq!(evaluator.scalar_calls, 0);
            assert!(workspace.is_idle());
            assert!(search.nodes.iter().all(|node| node.visits == 0));
            assert!(search.edges.iter().all(|edge| edge.visits == 0));
            assert_eq!(search.evaluations, 0);
            assert_eq!(pos.to_sfen(), original);
            assert!(history.matches(&pos));
            assert_eq!(history.len(), 1);
            assert_eq!(history.outcome(), None);
            assert!(search.path.is_empty());
        }
        Ok(())
    }

    #[test]
    fn nonroot_blocked_descendants_backtrack_to_a_root_sibling() -> Result<()> {
        let mut search = Search::new(Config {
            batch_size: 4,
            max_depth: 8,
            tree_memory_mb: 1,
            ..Config::default()
        })?;
        search.nodes.resize_with(6, Node::new);
        let mut pos = Position::new();
        pos.set_hirate();
        let original = pos.to_sfen();
        let first = resolved(&pos, "7g7f");
        let sibling = resolved(&pos, "2g2f");
        let mut reference = Position::new();
        reference.set_sfen(&original)?;
        reference.do_move(first, reference.gives_check(first));
        let reply = resolved(&reference, "7c7d");
        reference.do_move(reply, reference.gives_check(reply));
        let grandchild_moves = [resolved(&reference, "7f7e"), resolved(&reference, "2g2f")];
        search.edges.extend([
            edge(first, 1.0, 1),
            edge(sibling, 0.0, 2),
            edge(reply, 1.0, 3),
            edge(grandchild_moves[0], 0.5, 4),
            edge(grandchild_moves[1], 0.5, 5),
        ]);
        expand(&mut search, 0, 0, 2);
        expand(&mut search, 1, 2, 1);
        expand(&mut search, 3, 3, 2);
        let mut expected = Vec::new();
        let mut expected_sfen = Vec::new();
        for mv in grandchild_moves {
            reference.do_move(mv, reference.gives_check(mv));
            expected.push(rshogi_dl::tokens::encode(&reference)?);
            expected_sfen.push(reference.to_sfen());
            reference.undo_move(mv);
        }
        reference.set_sfen(&original)?;
        reference.do_move(sibling, reference.gives_check(sibling));
        expected.push(rshogi_dl::tokens::encode(&reference)?);
        expected_sfen.push(reference.to_sfen());
        let mut history = History::new(&pos);
        let stop = AtomicBool::new(false);
        let control = Control {
            limits: Limits {
                visits: 4,
                time: None,
            },
            stop: &stop,
            start: Instant::now(),
            history_len: history.len(),
        };
        let mut workspace = Workspace::new(InputFormat::StmTokensV1, 4, &search)?;
        for (row, path) in [&[0, 2, 3][..], &[0, 2, 4][..], &[1][..]].into_iter().enumerate() {
            assert!(matches!(
                collect(&mut search, &mut workspace, &mut pos, &mut history, &control)?,
                Collected::Neural
            ));
            assert_eq!(search.path, path);
            assert_eq!(pos.to_sfen(), expected_sfen[row]);
            assert_eq!(history.len(), control.history_len + path.len());
            assert!(history.matches(&pos));
            assert_eq!(workspace.inputs.view().row(row).unwrap().tokens(), Some(&expected[row]));
            restore_root(&mut search, &mut pos, &mut history, control.history_len);
            assert_eq!(pos.to_sfen(), original);
            assert_eq!(history.len(), control.history_len);
            assert!(history.matches(&pos));
            assert_eq!(history.outcome(), None);
        }
        assert_eq!(workspace.pending.len(), 3);
        assert!(workspace.node_blocked[1]);
        assert!(workspace.node_blocked[3]);
        assert_eq!(workspace.blocked, [3, 1]);
        assert_eq!(&workspace.edge_inflight[..5], &[2, 1, 2, 1, 1]);
        assert_eq!(&workspace.node_inflight[..6], &[3, 2, 1, 2, 1, 1]);
        assert!(workspace.reserved_edges > 0);
        workspace.cancel(&search);
        assert!(workspace.is_idle());
        assert!(search.path.is_empty());
        assert!(search.nodes.iter().all(|node| node.visits == 0));
        assert!(search.edges.iter().all(|edge| edge.visits == 0));
        Ok(())
    }

    #[test]
    fn repetition_at_a_leaf_is_separate_from_the_same_fresh_neural_input() -> Result<()> {
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b - 1")?;
        let leaf_tokens = rshogi_dl::tokens::encode(&pos)?;
        let mut repeated = History::new(&pos);
        for _ in 0..2 {
            for text in ["5i6i", "5a6a", "6i5i", "6a5a"] {
                let mv = resolved(&pos, text);
                pos.do_move(mv, pos.gives_check(mv));
                repeated.push(&pos);
            }
        }
        for text in ["5i6i", "5a6a", "6i5i"] {
            let mv = resolved(&pos, text);
            pos.do_move(mv, pos.gives_check(mv));
            repeated.push(&pos);
        }
        let original = pos.to_sfen();
        assert_eq!(repeated.len(), 12);
        assert_eq!(repeated.outcome(), None);
        for has_repetition in [true, false] {
            let mut history = if has_repetition {
                repeated.clone()
            } else {
                History::new(&pos)
            };
            let history_len = history.len();
            let mut search = Search::new(Config {
                batch_size: 4,
                tree_memory_mb: 1,
                ..Config::default()
            })?;
            search.nodes.resize_with(2, Node::new);
            search.edges.push(edge(resolved(&pos, "6a5a"), 1.0, 1));
            expand(&mut search, 0, 0, 1);
            let mut workspace = Workspace::new(InputFormat::StmTokensV1, 4, &search)?;
            let stop = AtomicBool::new(false);
            let control = Control {
                limits: Limits {
                    visits: 1,
                    time: None,
                },
                stop: &stop,
                start: Instant::now(),
                history_len,
            };
            let mut evaluator = Recorder::default();
            assert!(!run(
                &mut search,
                &mut pos,
                &mut history,
                &mut evaluator,
                &mut workspace,
                control
            )?);
            assert_eq!(
                search.nodes[1].outcome,
                if has_repetition {
                    Some(Outcome::Draw)
                } else {
                    None
                }
            );
            assert_eq!(search.nodes[0].visits, 1);
            assert_eq!(search.nodes[1].visits, 1);
            assert_eq!(search.evaluations, u64::from(!has_repetition));
            assert_eq!(evaluator.scalar_calls, 0);
            if has_repetition {
                assert!(evaluator.batches.is_empty());
            } else {
                assert_eq!(evaluator.batches, vec![vec![leaf_tokens]]);
            }
            assert!(workspace.is_idle());
            assert_eq!(pos.to_sfen(), original);
            assert!(history.matches(&pos));
            assert_eq!(history.len(), history_len);
            assert_eq!(history.outcome(), None);
            let mv = resolved(&pos, "6a5a");
            pos.do_move(mv, pos.gives_check(mv));
            history.push(&pos);
            assert_eq!(
                history.outcome(),
                if has_repetition {
                    Some(Outcome::Draw)
                } else {
                    None
                }
            );
            pos.undo_move(mv);
            history.truncate(history_len);
            assert_eq!(pos.to_sfen(), original);
            assert!(history.matches(&pos));
        }
        Ok(())
    }

    #[cfg(feature = "lazy-batch-metadata")]
    #[test]
    fn metadata_growth_is_bounded_and_preserves_live_reservations() -> Result<()> {
        let mut search = Search::new(Config {
            tree_memory_mb: 1,
            ..Config::default()
        })?;
        search.nodes.push(Node::new());
        let mut workspace = Workspace::new(InputFormat::StmTokensV1, 4, &search)?;
        assert_eq!(workspace.node_inflight.len(), 1);
        assert_eq!(workspace.node_blocked.len(), 1);
        assert!(workspace.edge_inflight.is_empty());
        workspace.node_inflight[0] = 2;
        workspace.block(0);
        for required in [2, 3, 7, 9, 33] {
            workspace.ensure_nodes(required, search.node_limit)?;
            assert!(workspace.node_inflight.len() >= required);
            assert!(workspace.node_inflight.len() <= required * 2);
            assert_eq!(workspace.node_inflight.len(), workspace.node_blocked.len());
            assert_eq!(workspace.node_inflight[0], 2);
            assert!(workspace.node_blocked[0]);
            assert!(workspace.node_inflight[1..].iter().all(|&count| count == 0));
            assert!(workspace.node_blocked[1..].iter().all(|&value| !value));
            assert_eq!(workspace.allocated_bytes(), workspace.measure_bytes()?);
        }
        for required in [1, 3, 33, 65, 257] {
            workspace.ensure_edges(required, search.edge_limit)?;
            assert!(workspace.edge_inflight.len() >= required);
            assert!(workspace.edge_inflight.len() <= required * 2);
            assert_eq!(workspace.allocated_bytes(), workspace.measure_bytes()?);
            workspace.edge_inflight[0] = 3;
        }
        let bytes = workspace.allocated_bytes();
        workspace.ensure_nodes(1, search.node_limit)?;
        workspace.ensure_edges(1, search.edge_limit)?;
        assert_eq!(workspace.node_inflight[0], 2);
        assert_eq!(workspace.edge_inflight[0], 3);
        assert_eq!(workspace.allocated_bytes(), bytes);
        workspace.node_inflight[0] = 0;
        workspace.edge_inflight[0] = 0;
        workspace.clear();
        assert!(workspace.is_idle());
        assert_eq!(workspace.allocated_bytes(), bytes);
        assert_eq!(growth_target(33, 63)?, 63);
        assert!(growth_target(0, 63).is_err());
        assert!(growth_target(64, 63).is_err());
        assert!(workspace.ensure_nodes(usize::MAX, usize::MAX).is_err());
        Ok(())
    }

    #[cfg(feature = "lazy-batch-metadata")]
    #[test]
    fn growth_failure_cancels_pending_and_restores_a_nonroot_path() -> Result<()> {
        use growth_fault::Point;
        for point in [Point::NodeInflight, Point::NodeBlocked, Point::EdgeInflight] {
            let mut search = Search::new(Config {
                batch_size: 4,
                tree_memory_mb: 1,
                ..Config::default()
            })?;
            let mut pos = Position::new();
            pos.set_hirate();
            let original = pos.to_sfen();
            let first = resolved(&pos, "7g7f");
            let mut reference = Position::new();
            reference.set_sfen(&original)?;
            reference.do_move(first, reference.gives_check(first));
            let reply = resolved(&reference, "7c7d");
            search.nodes.resize_with(2, Node::new);
            search.edges.push(edge(first, 1.0, 1));
            search.edges.push(Edge {
                child: None,
                ..edge(reply, 1.0, 1)
            });
            expand(&mut search, 0, 0, 1);
            expand(&mut search, 1, 1, 1);
            let mut workspace = Workspace::new(InputFormat::StmTokensV1, 4, &search)?;
            workspace.reserve(&search, 0, &[], LeafValue::Known(0.5));
            let mut history = History::new(&pos);
            let mut evaluator = Recorder::default();
            let stop = AtomicBool::new(false);
            let hook = growth_fault::Guard::arm(point);
            let error = run(
                &mut search,
                &mut pos,
                &mut history,
                &mut evaluator,
                &mut workspace,
                Control {
                    limits: Limits {
                        visits: 8,
                        time: None,
                    },
                    stop: &stop,
                    start: Instant::now(),
                    history_len: 1,
                },
            )
            .unwrap_err();
            assert_eq!(error.to_string(), "metadata allocation failed");
            assert_eq!(pos.to_sfen(), original);
            assert_eq!(history.len(), 1);
            assert!(history.matches(&pos));
            assert!(search.path.is_empty());
            assert!(workspace.is_idle());
            assert!(evaluator.batches.is_empty());
            assert_eq!(evaluator.scalar_calls, 0);
            assert_eq!(search.nodes[0].visits, 0);
            if point == Point::NodeBlocked {
                assert!(workspace.node_inflight.capacity() > workspace.node_inflight.len());
                assert_eq!(workspace.node_inflight.len(), 2);
                assert_eq!(workspace.node_blocked.len(), 2);
            }
            if point == Point::EdgeInflight {
                assert_eq!(search.nodes.len(), 3);
                assert!(search.edges[1].child.is_some());
            } else {
                assert_eq!(search.nodes.len(), 2);
                assert!(search.edges[1].child.is_none());
            }
            drop(hook);
        }
        Ok(())
    }

    #[cfg(feature = "lazy-batch-metadata")]
    #[test]
    fn failed_growth_drops_the_public_workspace_and_allows_a_fresh_run() -> Result<()> {
        for point in [
            growth_fault::Point::NodeInflight,
            growth_fault::Point::NodeBlocked,
            growth_fault::Point::EdgeInflight,
        ] {
            let mut search = Search::new(Config {
                batch_size: 4,
                tree_memory_mb: 1,
                ..Config::default()
            })?;
            let mut pos = Position::new();
            pos.set_hirate();
            let original = pos.to_sfen();
            let mut history = History::new(&pos);
            let mut evaluator = Recorder::default();
            let stop = AtomicBool::new(false);
            let limits = Limits {
                visits: 16,
                time: None,
            };
            let hook = growth_fault::Guard::arm(point);
            let error =
                search.run(&mut pos, &mut history, &mut evaluator, limits, &stop).unwrap_err();
            assert_eq!(error.to_string(), "metadata allocation failed");
            assert!(search.batch.is_none());
            assert_eq!(search.batch_workspace_bytes(), 0);
            assert_eq!(pos.to_sfen(), original);
            assert_eq!(history.len(), 1);
            assert!(history.matches(&pos));
            assert_eq!(evaluator.scalar_calls, 0);
            assert_eq!(
                evaluator.batches.len(),
                usize::from(point != growth_fault::Point::EdgeInflight)
            );
            drop(hook);
            let result = search.run(&mut pos, &mut history, &mut evaluator, limits, &stop)?;
            assert_eq!(result.visits, 16);
            let bytes = search.batch_workspace_bytes();
            let retained = search.batch.as_ref().unwrap();
            assert!(retained.is_idle());
            assert_eq!(bytes, retained.measure_bytes()?);
            let result = search.run(
                &mut pos,
                &mut history,
                &mut evaluator,
                Limits {
                    visits: 1,
                    time: None,
                },
                &stop,
            )?;
            assert_eq!(result.visits, 1);
            assert_eq!(search.batch_workspace_bytes(), bytes);
        }
        Ok(())
    }

    #[cfg(feature = "lazy-batch-metadata")]
    #[test]
    fn allocation_fault_guard_clears_after_drop_and_unwind() {
        {
            let guard = growth_fault::Guard::arm(growth_fault::Point::EdgeInflight);
            assert!(growth_fault::check(growth_fault::Point::EdgeInflight).is_err());
            drop(guard);
        }
        assert!(growth_fault::check(growth_fault::Point::EdgeInflight).is_ok());
        let result = std::panic::catch_unwind(|| {
            let guard = growth_fault::Guard::arm(growth_fault::Point::NodeBlocked);
            assert!(guard.is_armed());
            assert!(growth_fault::check(growth_fault::Point::NodeBlocked).is_err());
            panic!("exercise allocation hook cleanup");
        });
        assert!(result.is_err());
        assert!(growth_fault::check(growth_fault::Point::NodeBlocked).is_ok());
    }

    #[test]
    fn workspace_size_products_are_checked_without_allocating() -> Result<()> {
        assert!(elements(usize::MAX, 2).is_err());
        assert!(filled(usize::MAX, 0u8).is_err());
        let search = Search::new(Config::default())?;
        for capacity in [0, 1, 1025, usize::MAX] {
            assert!(Workspace::new(InputFormat::StmTokensV1, capacity, &search).is_err());
        }
        Ok(())
    }
}
