//! **MLFQ**: a multilevel feedback queue scheduling policy.
//!
//! **Status: implemented and tested, not currently wired into the kernel.**
//! It was wired in and booted, and behaved as designed, but MLFQ infers
//! priority from a task giving the CPU up early and the kernel has no
//! blocking primitive: no sleep, no wait queues, no blocked state. Waiting
//! tasks poll, so "yielded" really means "still runnable," and the serial
//! shell busy-yielded its way to 174,729 context switches in 30 seconds.
//! The prerequisite is a blocked task state and a wakeup path. Until then
//! the kernel keeps round-robin and this crate stays ready. See the
//! scheduling note in README.md.
//!
//! Round-robin is fair but has no notion of *behaviour*. A task that blocks
//! on input after 200 microseconds and a task that busy-loops forever get
//! identical treatment, so interactive work waits behind whatever CPU hog
//! happens to be ahead of it. MLFQ infers priority from observed behaviour
//! rather than asking anyone to declare it:
//!
//! 1. Several priority levels, each with its own quantum. The queues run
//!    strictly highest-first, round-robin within a level.
//! 2. A task that burns its entire quantum is treated as CPU-bound and
//!    demoted one level, where it will run less often but for longer when
//!    it does.
//! 3. A task that yields before its quantum expires is treated as
//!    interactive and keeps its level.
//! 4. Every `boost_interval` ticks, everything is lifted back to the top
//!    level. Without this, a long-running CPU task that has sunk to the
//!    bottom can be starved indefinitely by a stream of short tasks, and a
//!    task that *becomes* interactive after a CPU-heavy phase never
//!    recovers. This is the classic starvation guard, and the test below
//!    asserts it.
//!
//! Deliberately free of any TCB, register, or assembly dependency: this
//! decides *which task index runs next and for how long*, nothing else. The
//! kernel owns the task table and the context switch. Keeping the policy
//! separate is what lets starvation and demotion be tested on the host
//! instead of inferred from a serial log.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// Priority levels. Three is the usual starting point: interactive,
/// mixed, and CPU-bound.
pub const LEVELS: usize = 3;

/// Quantum for each level, in timer ticks. Lower priority levels get longer
/// slices, so a CPU-bound task pays less context-switch overhead per unit
/// of work even though it is scheduled less often.
pub const LEVEL_QUANTA: [u64; LEVELS] = [2, 4, 8];

/// Ticks between priority boosts. At 100Hz this is half a second, short
/// enough that a starved task recovers promptly and long enough that the
/// boost does not erase the information demotion just gathered.
pub const DEFAULT_BOOST_INTERVAL: u64 = 50;

/// Why a task stopped running, which is the signal MLFQ infers priority
/// from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// Ran the quantum out. Treated as CPU-bound: demote.
    QuantumExpired,
    /// Gave the CPU up early (yielded, or blocked waiting for input).
    /// Treated as interactive: keep the level.
    Yielded,
}

pub struct Mlfq {
    queues: [VecDeque<usize>; LEVELS],
    /// Level of each task, indexed by task index. `None` means the task is
    /// not currently managed (never added, or removed).
    level_of: Vec<Option<u8>>,
    boost_interval: u64,
    last_boost_tick: u64,
    boosts: u64,
    demotions: u64,
}

impl Default for Mlfq {
    fn default() -> Self {
        Self::new(DEFAULT_BOOST_INTERVAL)
    }
}

impl Mlfq {
    pub fn new(boost_interval: u64) -> Self {
        Self {
            queues: core::array::from_fn(|_| VecDeque::new()),
            level_of: Vec::new(),
            boost_interval,
            last_boost_tick: 0,
            boosts: 0,
            demotions: 0,
        }
    }

    /// Priority boosts performed since construction.
    pub fn boosts(&self) -> u64 {
        self.boosts
    }

    /// Demotions performed since construction.
    pub fn demotions(&self) -> u64 {
        self.demotions
    }

    /// Current level of a task, if it is being managed.
    pub fn level(&self, task: usize) -> Option<u8> {
        self.level_of.get(task).copied().flatten()
    }

    /// Quantum a task should get for its next run, in ticks.
    pub fn quantum(&self, task: usize) -> u64 {
        LEVEL_QUANTA[self.level(task).unwrap_or(0) as usize]
    }

    pub fn is_empty(&self) -> bool {
        self.queues.iter().all(|q| q.is_empty())
    }

    pub fn len(&self) -> usize {
        self.queues.iter().map(|q| q.len()).sum()
    }

    fn set_level(&mut self, task: usize, level: u8) {
        if self.level_of.len() <= task {
            self.level_of.resize(task + 1, None);
        }
        self.level_of[task] = Some(level);
    }

    /// Adds a task at the top priority level. New tasks start highest on
    /// the assumption they may be interactive; if they are not, demotion
    /// finds out within a quantum or two.
    pub fn add(&mut self, task: usize) {
        if self.queues.iter().any(|q| q.contains(&task)) {
            return; // already queued
        }
        self.set_level(task, 0);
        self.queues[0].push_back(task);
    }

    /// Removes a task entirely (it exited).
    pub fn remove(&mut self, task: usize) {
        for q in self.queues.iter_mut() {
            q.retain(|&t| t != task);
        }
        if let Some(slot) = self.level_of.get_mut(task) {
            *slot = None;
        }
    }

    /// Takes the next task to run: highest non-empty level, round-robin
    /// within it.
    pub fn pick_next(&mut self) -> Option<usize> {
        for q in self.queues.iter_mut() {
            if let Some(task) = q.pop_front() {
                return Some(task);
            }
        }
        None
    }

    /// Re-queues a task that has stopped running, applying the feedback
    /// rule for why it stopped.
    pub fn requeue(&mut self, task: usize, stop: Stop) {
        let level = self.level(task).unwrap_or(0);
        let new_level = match stop {
            // Demote, saturating at the bottom level.
            Stop::QuantumExpired => {
                let demoted = (level + 1).min(LEVELS as u8 - 1);
                if demoted != level {
                    self.demotions += 1;
                }
                demoted
            }
            Stop::Yielded => level,
        };
        self.set_level(task, new_level);
        self.queues[new_level as usize].push_back(task);
    }

    /// Lifts every queued task back to the top level. Called on the boost
    /// interval; also usable directly.
    pub fn boost(&mut self) {
        self.boosts += 1;
        let mut moved = Vec::new();
        for q in self.queues.iter_mut().skip(1) {
            while let Some(task) = q.pop_front() {
                moved.push(task);
            }
        }
        for task in moved {
            self.set_level(task, 0);
            self.queues[0].push_back(task);
        }
    }

    /// Boosts if enough ticks have passed since the last one. Returns
    /// whether a boost happened.
    pub fn maybe_boost(&mut self, now: u64) -> bool {
        if now.saturating_sub(self.last_boost_tick) >= self.boost_interval {
            self.last_boost_tick = now;
            self.boost();
            return true;
        }
        false
    }

    /// Tasks queued at each level, for reporting.
    pub fn level_counts(&self) -> [usize; LEVELS] {
        core::array::from_fn(|i| self.queues[i].len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A boost interval large enough that boosting never interferes, for
    /// tests that want to observe demotion in isolation.
    fn no_boost() -> Mlfq {
        Mlfq::new(u64::MAX)
    }

    #[test]
    fn new_tasks_start_at_the_top_level() {
        let mut m = no_boost();
        m.add(7);
        assert_eq!(m.level(7), Some(0));
        assert_eq!(m.quantum(7), LEVEL_QUANTA[0]);
        assert_eq!(m.pick_next(), Some(7));
    }

    #[test]
    fn adding_the_same_task_twice_is_a_no_op() {
        let mut m = no_boost();
        m.add(1);
        m.add(1);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn picks_higher_levels_first() {
        let mut m = no_boost();
        m.add(1);
        m.add(2);
        // Push 1 down to level 1, leaving 2 alone at level 0.
        m.pick_next();
        m.requeue(1, Stop::QuantumExpired);
        assert_eq!(m.level(1), Some(1));

        assert_eq!(m.pick_next(), Some(2), "level 0 must win");
        assert_eq!(m.pick_next(), Some(1));
    }

    #[test]
    fn round_robins_within_a_level() {
        let mut m = no_boost();
        for t in 0..3 {
            m.add(t);
        }
        // Everyone yields, so nobody is demoted and all stay at level 0.
        let mut order = Vec::new();
        for _ in 0..6 {
            let t = m.pick_next().unwrap();
            order.push(t);
            m.requeue(t, Stop::Yielded);
        }
        assert_eq!(order, [0, 1, 2, 0, 1, 2], "not round-robin within level");
        assert!((0..3).all(|t| m.level(t) == Some(0)));
    }

    #[test]
    fn burning_the_quantum_demotes_one_level_at_a_time() {
        let mut m = no_boost();
        m.add(0);
        for expected in 1..LEVELS as u8 {
            m.pick_next();
            m.requeue(0, Stop::QuantumExpired);
            assert_eq!(m.level(0), Some(expected));
        }
        // Already at the bottom: further demotion saturates.
        let before = m.demotions();
        m.pick_next();
        m.requeue(0, Stop::QuantumExpired);
        assert_eq!(m.level(0), Some(LEVELS as u8 - 1));
        assert_eq!(m.demotions(), before, "saturating demote must not count");
    }

    #[test]
    fn yielding_preserves_priority() {
        let mut m = no_boost();
        m.add(0);
        for _ in 0..10 {
            let t = m.pick_next().unwrap();
            m.requeue(t, Stop::Yielded);
        }
        assert_eq!(m.level(0), Some(0), "an interactive task must stay high");
        assert_eq!(m.demotions(), 0);
    }

    #[test]
    fn lower_levels_get_longer_quanta() {
        // The whole point of demotion: run less often, but longer each time.
        assert!(LEVEL_QUANTA.windows(2).all(|w| w[1] > w[0]));
        let mut m = no_boost();
        m.add(0);
        let top = m.quantum(0);
        m.pick_next();
        m.requeue(0, Stop::QuantumExpired);
        assert!(m.quantum(0) > top);
    }

    #[test]
    fn boost_lifts_everything_back_to_the_top() {
        let mut m = no_boost();
        for t in 0..3 {
            m.add(t);
        }
        for _ in 0..LEVELS * 2 {
            if let Some(t) = m.pick_next() {
                m.requeue(t, Stop::QuantumExpired);
            }
        }
        assert!((0..3).any(|t| m.level(t) != Some(0)), "expected demotions");

        m.boost();
        assert!((0..3).all(|t| m.level(t) == Some(0)), "boost missed a task");
        assert_eq!(m.level_counts()[0], 3);
    }

    #[test]
    fn maybe_boost_respects_the_interval() {
        let mut m = Mlfq::new(50);
        m.add(0);
        assert!(!m.maybe_boost(10), "too early");
        assert!(m.maybe_boost(50), "interval reached");
        assert!(!m.maybe_boost(60), "just boosted");
        assert!(m.maybe_boost(100), "next interval");
        assert_eq!(m.boosts(), 2);
    }

    #[test]
    fn removing_a_task_drops_it_from_every_level() {
        let mut m = no_boost();
        m.add(0);
        m.add(1);
        m.pick_next();
        m.requeue(0, Stop::QuantumExpired); // 0 now at level 1
        m.remove(0);
        assert_eq!(m.level(0), None);
        assert_eq!(m.len(), 1);
        assert_eq!(m.pick_next(), Some(1));
        assert_eq!(m.pick_next(), None);
    }

    /// The property the boost exists for. A CPU-bound task sinks to the
    /// bottom; a stream of interactive tasks then monopolizes the top
    /// level. Without boosting, the CPU-bound task never runs again. With
    /// it, the task keeps getting service.
    #[test]
    fn boosting_prevents_starvation_of_a_cpu_bound_task() {
        const HOG: usize = 99;
        let mut m = Mlfq::new(20);
        m.add(HOG);

        // Sink the hog to the bottom level.
        while m.level(HOG) != Some(LEVELS as u8 - 1) {
            let t = m.pick_next().unwrap();
            m.requeue(t, Stop::QuantumExpired);
        }

        // Now run a workload of well-behaved interactive tasks alongside it.
        for t in 0..5 {
            m.add(t);
        }

        let mut hog_runs = 0;
        for tick in 0..400u64 {
            m.maybe_boost(tick);
            if let Some(t) = m.pick_next() {
                if t == HOG {
                    hog_runs += 1;
                    m.requeue(t, Stop::QuantumExpired);
                } else {
                    m.requeue(t, Stop::Yielded);
                }
            }
        }
        assert!(m.boosts() > 0, "no boosts happened");
        assert!(
            hog_runs > 0,
            "the CPU-bound task starved despite priority boosting"
        );
    }

    /// The same workload with boosting effectively disabled must starve the
    /// hog. This is the control: without it, the test above could pass for
    /// reasons unrelated to boosting.
    #[test]
    fn without_boosting_the_cpu_bound_task_does_starve() {
        const HOG: usize = 99;
        let mut m = no_boost();
        m.add(HOG);
        while m.level(HOG) != Some(LEVELS as u8 - 1) {
            let t = m.pick_next().unwrap();
            m.requeue(t, Stop::QuantumExpired);
        }
        for t in 0..5 {
            m.add(t);
        }

        let mut hog_runs = 0;
        for _ in 0..400 {
            if let Some(t) = m.pick_next() {
                if t == HOG {
                    hog_runs += 1;
                    m.requeue(t, Stop::QuantumExpired);
                } else {
                    m.requeue(t, Stop::Yielded);
                }
            }
        }
        assert_eq!(hog_runs, 0, "control failed: hog ran without boosting");
    }

    /// Every task added must eventually run, and the queue must never lose
    /// or duplicate one, under a mixed randomized workload.
    #[test]
    fn no_task_is_lost_or_duplicated_under_churn() {
        const N: usize = 12;
        let mut m = Mlfq::new(37);
        for t in 0..N {
            m.add(t);
        }

        let mut rng: u64 = 0x00C0_FFEE_1234_5678;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };

        let mut ran = [0usize; N];
        for tick in 0..20_000u64 {
            m.maybe_boost(tick);
            let Some(t) = m.pick_next() else {
                panic!("queue went empty at tick {tick} with {N} live tasks");
            };
            ran[t] += 1;
            let stop = if next() % 2 == 0 {
                Stop::QuantumExpired
            } else {
                Stop::Yielded
            };
            m.requeue(t, stop);
            assert_eq!(m.len(), N, "tick {tick}: task lost or duplicated");
        }
        assert!(
            ran.iter().all(|&c| c > 0),
            "some task never ran at all: {ran:?}"
        );
    }
}
