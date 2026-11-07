use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use tools::selfplay::{
    ParsedPosition, ensure_start_positions_within_max_moves, load_start_positions,
};

pub(super) struct CyclePosition {
    pub name: String,
    pub command: String,
    pub parsed: ParsedPosition,
}

fn parse_positions(content: &str, max_moves: u32) -> Result<Vec<Arc<CyclePosition>>> {
    let mut positions = Vec::new();
    let mut names = HashSet::new();
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let (name, payload) = match line.split_once('\t') {
            Some((name, payload)) => {
                if name.trim().is_empty() {
                    bail!("empty startpos name on line {}", index + 1);
                }
                (Some(name.trim()), payload)
            }
            None => (None, line),
        };
        let (mut defs, commands) = load_start_positions(None, Some(payload.trim()), None, None)
            .with_context(|| format!("invalid startpos on line {}", index + 1))?;
        defs[0].source_line = Some(index + 1);
        ensure_start_positions_within_max_moves(&defs, max_moves)?;
        let command = commands.into_iter().next().expect("one position");
        let name = name.map(str::to_owned).unwrap_or_else(|| {
            format!("{:x}", Sha256::digest(command.as_bytes()))[..12].to_owned()
        });
        if !names.insert(name.clone()) {
            bail!("duplicate startpos name on line {}: {name}", index + 1);
        }
        positions.push(Arc::new(CyclePosition {
            name,
            command,
            parsed: defs.remove(0),
        }));
    }
    if positions.is_empty() {
        bail!("no usable start positions");
    }
    Ok(positions)
}

pub(super) struct CycleState {
    path: PathBuf,
    max_moves: u32,
    signature: (SystemTime, u64),
    sha256: String,
    checked: Instant,
    warning: Option<String>,
    pub positions: Vec<Arc<CyclePosition>>,
    pub counts: Vec<HashMap<String, u64>>,
    pub open_pairs: HashMap<usize, (usize, Arc<CyclePosition>)>,
}

impl CycleState {
    pub fn load(path: PathBuf, max_moves: u32) -> Result<Self> {
        let metadata = std::fs::metadata(&path)?;
        let content = std::fs::read_to_string(&path)?;
        Ok(Self {
            positions: parse_positions(&content, max_moves)?,
            signature: (metadata.modified()?, metadata.len()),
            sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
            path,
            max_moves,
            checked: Instant::now(),
            warning: None,
            counts: Vec::new(),
            open_pairs: HashMap::new(),
        })
    }

    pub fn refresh(&mut self) {
        if self.checked.elapsed() < Duration::from_secs(5) {
            return;
        }
        self.checked = Instant::now();
        match self.reload() {
            Ok(()) => self.warning = None,
            Err(error) => {
                let warning = format!("{error:#}");
                if self.warning.as_ref() != Some(&warning) {
                    eprintln!("[startpos] warning: {warning}; keeping previous positions");
                    self.warning = Some(warning);
                }
            }
        }
    }

    fn reload(&mut self) -> Result<()> {
        let metadata = std::fs::metadata(&self.path)?;
        let signature = (metadata.modified()?, metadata.len());
        if signature == self.signature {
            return Ok(());
        }
        let content = std::fs::read_to_string(&self.path)?;
        let sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
        if sha256 != self.sha256 {
            let positions = parse_positions(&content, self.max_moves)?;
            let old: HashSet<_> = self.positions.iter().map(|p| &p.name).collect();
            let new: HashSet<_> = positions.iter().map(|p| &p.name).collect();
            let added = new.difference(&old).count();
            let removed = old.difference(&new).count();
            for counts in &mut self.counts {
                let minimum = positions
                    .iter()
                    .filter_map(|p| counts.get(&p.name))
                    .min()
                    .copied()
                    .unwrap_or(0);
                for position in &positions {
                    counts.entry(position.name.clone()).or_insert(minimum);
                }
            }
            self.positions = positions;
            self.sha256 = sha256;
            eprintln!(
                "[startpos] reloaded sha256={} positions={} added={added} removed={removed}",
                self.sha256,
                self.positions.len()
            );
        }
        self.signature = signature;
        Ok(())
    }

    pub fn select(&mut self, card: usize) -> (usize, Arc<CyclePosition>) {
        if self.counts.len() <= card {
            self.counts.resize_with(card + 1, HashMap::new);
        }
        let counts = &mut self.counts[card];
        for position in &self.positions {
            counts.entry(position.name.clone()).or_default();
        }
        let (index, position) = self
            .positions
            .iter()
            .enumerate()
            .min_by_key(|(_, p)| counts[&p.name])
            .expect("nonempty positions");
        (index, position.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MatchResult, MatchTicket, StartposOrder, TicketSource};
    use std::cell::RefCell;
    use std::sync::atomic::AtomicU32;
    use tools::selfplay::GameOutcome;

    const THREE: &str =
        "a\tposition startpos\nb\tposition startpos moves 7g7f\nc\tposition startpos moves 2g2f\n";

    fn source(path: PathBuf) -> TicketSource {
        let mut source = TicketSource::new(
            vec![(0, 1), (0, 2)],
            3,
            Arc::new(AtomicU32::new(100)),
            0,
            StartposOrder::Cycle,
        );
        source.cycle = Some(RefCell::new(CycleState::load(path, 512).unwrap()));
        source
    }

    fn pull(source: &mut TicketSource) -> MatchTicket {
        let ticket = source.peek_ticket().unwrap();
        source.commit_sent(&ticket);
        ticket
    }

    fn update(source: &TicketSource, content: &str) {
        let mut cycle = source.cycle.as_ref().unwrap().borrow_mut();
        std::fs::write(&cycle.path, content).unwrap();
        cycle.checked = Instant::now() - Duration::from_secs(6);
        cycle.refresh();
    }

    #[test]
    fn names_hashes_tabs_and_validation() {
        let positions = parse_positions(
            "# comment\n\nopening\tposition\tstartpos moves\t7g7f\nposition  startpos\n",
            512,
        )
        .unwrap();
        assert_eq!(positions[0].name, "opening");
        assert_eq!(positions[0].command, "position startpos moves 7g7f");
        assert_eq!(positions[1].name, &format!("{:x}", Sha256::digest(b"position startpos"))[..12]);
        let raw = format!("{}\n", rshogi_core::position::SFEN_HIRATE);
        assert!(parse_positions(&raw, 512).unwrap()[0].command.starts_with("position sfen "));
        for invalid in [
            "a\tposition startpos\na\tposition startpos moves 7g7f",
            "position startpos\nposition  startpos",
            "\tposition startpos",
            "a\t",
            "a\tposition startpos moves invalid",
            "# empty",
        ] {
            assert!(parse_positions(invalid, 512).is_err(), "{invalid:?}");
        }
        assert!(parse_positions("a\tposition startpos moves 7g7f", 1).is_err());
    }

    #[test]
    fn uniform_cycle_add_remove_and_restore_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("positions");
        std::fs::write(&path, THREE).unwrap();
        let mut source = source(path);
        for _ in 0..9 {
            pull(&mut source);
            pull(&mut source);
        }
        assert!(source.cycle.as_ref().unwrap().borrow().counts[0].values().all(|&n| n == 3));
        update(&source, &format!("{THREE}d\tposition startpos moves 7g7f 3c3d\n"));
        assert_eq!(source.cycle.as_ref().unwrap().borrow().counts[0]["d"], 3);
        let next = pull(&mut source);
        assert_eq!(next.cycle_position.unwrap().name, "a");
        pull(&mut source);
        update(&source, "b\tposition startpos moves 7g7f\n");
        for _ in 0..3 {
            assert_eq!(pull(&mut source).cycle_position.unwrap().name, "b");
            pull(&mut source);
        }
        update(&source, THREE);
        assert_eq!(source.cycle.as_ref().unwrap().borrow().counts[0]["a"], 4);
        assert_eq!(source.cycle.as_ref().unwrap().borrow().counts[0]["b"], 6);
        assert_eq!(pull(&mut source).cycle_position.unwrap().name, "c");
        // 別カードには独立した組数を使う。
        let mut cycle = source.cycle.as_ref().unwrap().borrow_mut();
        assert_eq!(cycle.select(1).1.name, "a");
        assert_eq!(cycle.counts[0]["b"], 6);
    }

    #[test]
    fn reload_failure_keeps_positions_and_pair_and_retry_keep_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("positions");
        std::fs::write(&path, THREE).unwrap();
        let mut source = source(path);
        let first = pull(&mut source);
        for invalid in ["", "bad\tposition startpos moves illegal"] {
            update(&source, invalid);
            assert_eq!(source.cycle.as_ref().unwrap().borrow().positions.len(), 3);
            assert!(source.cycle.as_ref().unwrap().borrow().warning.is_some());
        }
        update(&source, "replacement\tposition startpos moves 2g2f\n");
        let second = pull(&mut source);
        let original = first.cycle_position.as_ref().unwrap();
        assert!(Arc::ptr_eq(original, second.cycle_position.as_ref().unwrap()));
        for ticket in [first.clone(), second] {
            let error = ticket.pair_slot == 0;
            source.observe_result(&MatchResult {
                ticket,
                outcome: GameOutcome::Draw,
                reason: "error".into(),
                plies: 0,
                move_logs: Vec::new(),
                error,
                worker_retired: false,
            });
        }
        for _ in 0..2 {
            let retry = pull(&mut source);
            assert_eq!(retry.attempt, 1);
            assert!(Arc::ptr_eq(original, retry.cycle_position.as_ref().unwrap()));
        }
        assert_eq!(source.cycle.as_ref().unwrap().borrow().counts[0]["a"], 1);
        assert_eq!(pull(&mut source).cycle_position.unwrap().name, "replacement");
    }

    #[test]
    fn pending_first_game_refreshes_without_counting_until_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("positions");
        std::fs::write(&path, THREE).unwrap();
        let mut source = source(path.clone());
        let mut pending = None;
        assert_eq!(
            source.offer_ticket(&mut pending, false).unwrap().cycle_position.unwrap().name,
            "a"
        );
        assert_eq!(
            source.offer_ticket(&mut pending, false).unwrap().cycle_position.unwrap().name,
            "a"
        );
        assert_eq!(source.cycle.as_ref().unwrap().borrow().counts[0]["a"], 0);
        std::fs::write(&path, "new\tposition startpos moves 7g7f\n").unwrap();
        source.cycle.as_ref().unwrap().borrow_mut().checked =
            Instant::now() - Duration::from_secs(6);
        let ticket = source.offer_ticket(&mut pending, false).unwrap();
        assert_eq!(ticket.cycle_position.as_ref().unwrap().name, "new");
        source.commit_sent(&ticket);
        assert_eq!(source.cycle.as_ref().unwrap().borrow().counts[0]["new"], 1);
        assert!(Arc::ptr_eq(
            ticket.cycle_position.as_ref().unwrap(),
            pull(&mut source).cycle_position.as_ref().unwrap()
        ));
    }

    #[test]
    fn refresh_is_throttled_and_identical_content_keeps_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("positions");
        std::fs::write(&path, THREE).unwrap();
        let mut cycle = CycleState::load(path.clone(), 512).unwrap();
        let original = cycle.positions[0].clone();
        cycle.signature = (SystemTime::UNIX_EPOCH, 0);
        cycle.reload().unwrap();
        assert!(Arc::ptr_eq(&original, &cycle.positions[0]));
        std::fs::write(&path, "replacement\tposition startpos moves 7g7f\n").unwrap();
        cycle.refresh();
        assert!(Arc::ptr_eq(&original, &cycle.positions[0]));
        cycle.checked = Instant::now() - Duration::from_secs(6);
        cycle.refresh();
        assert_eq!(cycle.positions[0].name, "replacement");
        std::fs::remove_file(path).unwrap();
        cycle.checked = Instant::now() - Duration::from_secs(6);
        cycle.refresh();
        assert_eq!(cycle.positions[0].name, "replacement");
        assert!(cycle.warning.is_some());
    }

    #[test]
    fn result_fields_only_for_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("positions");
        std::fs::write(&path, THREE).unwrap();
        let mut source = source(path);
        let ticket = pull(&mut source);
        for cycle in [true, false] {
            let mut ticket = ticket.clone();
            if !cycle {
                ticket.cycle_position = None;
            }
            let output = dir.path().join(format!("{cycle}.jsonl"));
            let writer = crate::PairWriter::new(&output).unwrap();
            let mut writers = HashMap::from([((0, 1), writer)]);
            crate::process_result(
                &MatchResult {
                    ticket,
                    outcome: GameOutcome::Draw,
                    reason: "max_moves".into(),
                    plies: 1,
                    move_logs: Vec::new(),
                    error: false,
                    worker_retired: false,
                },
                &["a".into(), "b".into()],
                &mut writers,
                &mut HashMap::new(),
            )
            .unwrap();
            let value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(output).unwrap()).unwrap();
            if cycle {
                assert_eq!(value["startpos"], "position startpos");
                assert_eq!(value["startpos_name"], "a");
            } else {
                assert!(value.get("startpos").is_none());
                assert!(value.get("startpos_name").is_none());
            }
        }
    }
}
