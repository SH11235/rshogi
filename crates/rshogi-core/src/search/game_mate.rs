use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::mate::dfpn::game::GameMate;
use crate::mate::dfpn::{DfpnSolver, HELPER_STACK_SIZE, MateMode};
use crate::position::Position;
use crate::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Config {
    pub threads: usize,
    pub hash_mb: usize,
    pub mode: MateMode,
}

struct ResultSlot {
    closed: bool,
    mate: Option<GameMate>,
}

impl ResultSlot {
    fn publish(&mut self, mate: GameMate, deadline: Instant, main_stop: &AtomicBool) {
        if !self.closed
            && Instant::now() < deadline
            && main_stop
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            self.mate = Some(mate);
        }
    }
}

struct Job {
    config: Config,
    cancel: Arc<AtomicBool>,
    slot: Arc<Mutex<ResultSlot>>,
    thread: JoinHandle<Option<DfpnSolver>>,
}

#[derive(Default)]
pub(super) struct GameMateSearch {
    job: Option<Job>,
    solver: Option<(Config, DfpnSolver)>,
}

impl GameMateSearch {
    pub(super) fn reap(&mut self) {
        if self.job.as_ref().is_some_and(|job| job.thread.is_finished())
            && let Some(job) = self.job.take()
        {
            self.solver = job.thread.join().ok().flatten().map(|solver| (job.config, solver));
        }
    }

    pub(super) fn busy_threads(&self) -> usize {
        self.job.as_ref().map_or(0, |job| job.config.threads)
    }

    pub(super) fn ensure_thread_limit(&mut self, total: usize) {
        self.reap();
        if self.busy_threads() >= total {
            self.drain();
        }
    }

    pub(super) fn drain(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Release);
            self.solver = job.thread.join().ok().flatten().map(|solver| (job.config, solver));
        }
    }

    pub(super) fn start(
        &mut self,
        config: Config,
        pos: &Position,
        max_moves: i32,
        deadline: Instant,
        main_stop: Arc<AtomicBool>,
    ) -> bool {
        if self.job.is_some() || main_stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            return false;
        }
        let mut pos = pos.clone();
        let cached = self.solver.take();
        let slot = Arc::new(Mutex::new(ResultSlot {
            closed: false,
            mate: None,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_slot = Arc::clone(&slot);
        let worker_cancel = Arc::clone(&cancel);
        let thread = thread::Builder::new()
            .name("game-mate".into())
            .stack_size(HELPER_STACK_SIZE)
            .spawn(move || {
                let cached = cached.filter(|(previous, _)| {
                    previous.hash_mb == config.hash_mb && previous.mode == config.mode
                });
                if worker_cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return cached.map(|(_, solver)| solver);
                }
                let mut solver = cached.map_or_else(
                    || DfpnSolver::with_mode(config.hash_mb, config.mode),
                    |(_, solver)| solver,
                );
                solver.set_threads(config.threads);
                if let Some(mate) = solver.solve_game(&mut pos, max_moves, deadline, &worker_cancel)
                {
                    worker_slot.lock().unwrap().publish(mate, deadline, &main_stop);
                }
                Some(solver)
            });
        let Ok(thread) = thread else {
            return false;
        };
        self.job = Some(Job {
            config,
            cancel,
            slot,
            thread,
        });
        true
    }

    pub(super) fn finish(&mut self) -> Option<GameMate> {
        let result = self.job.as_ref().and_then(|job| {
            let mut slot = job.slot.lock().unwrap();
            slot.closed = true;
            job.cancel.store(true, Ordering::Release);
            slot.mate.take()
        });
        self.reap();
        result
    }
}

impl Drop for GameMateSearch {
    fn drop(&mut self) {
        self.finish();
        self.drain();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Move;
    use std::time::Duration;

    #[test]
    fn publishes_only_before_cancellation_or_deadline() {
        let mut slot = ResultSlot {
            closed: false,
            mate: None,
        };
        let stop = AtomicBool::new(true);
        let future = Instant::now() + Duration::from_secs(10);
        let result = || GameMate {
            first: Move::NONE,
            plies: 1,
        };

        slot.publish(result(), future, &stop);
        assert!(slot.mate.is_none());

        stop.store(false, Ordering::Release);
        slot.publish(result(), Instant::now(), &stop);
        assert!(slot.mate.is_none());
        assert!(!stop.load(Ordering::Acquire));

        slot.closed = true;
        slot.publish(result(), future, &stop);
        assert!(slot.mate.is_none());
        assert!(!stop.load(Ordering::Acquire));

        slot.closed = false;
        slot.publish(result(), future, &stop);
        assert!(slot.mate.is_some());
        assert!(stop.load(Ordering::Acquire));
    }

    #[test]
    fn worker_delivers_a_verified_move_once() {
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/4P4/9/9/9/9/9/4K4 b G 1").unwrap();
        let mut game_mate = GameMateSearch::default();
        let stop = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + Duration::from_secs(10);
        assert!(game_mate.start(
            Config {
                threads: 1,
                hash_mb: 1,
                mode: MateMode::Power
            },
            &pos,
            512,
            deadline,
            Arc::clone(&stop)
        ));
        while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(game_mate.finish().unwrap().first.to_usi(), "G*5b");
        assert!(game_mate.finish().is_none());
        game_mate.drain();
        assert_eq!(game_mate.busy_threads(), 0);
    }
}
