use super::context_switch::switch_to;
use super::tcb::{alloc_task_stack, prepare_initial_stack, TaskId, TaskState, Tcb};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;

/// Round-robin ready queue (the classic circular-queue OS scheduling
/// structure) over a flat task table. `ready_queue` holds indices into
/// `tasks`, not `Tcb`s themselves, so switching order is just a rotation
/// of small integers.
struct SchedulerState {
    tasks: Vec<Tcb>,
    ready_queue: VecDeque<usize>,
    current: usize,
    /// Tick at which the running task was last credited, so `ticks_run` can
    /// be charged by actual elapsed time rather than an assumed full slice.
    last_accounted_tick: u64,
}

static SCHEDULER: Mutex<Option<SchedulerState>> = Mutex::new(None);

/// Sets up the scheduler with the *currently executing* context (whatever
/// called this) as task 0, so the first real context switch has somewhere
/// valid to save its registers into.
pub fn init() {
    let boot_task = Tcb {
        id: 0,
        saved_rsp: 0, // filled in by the first switch_to that saves *out of* task 0
        state: TaskState::Running,
        switches: 0,
        ticks_run: 0,
    };
    *SCHEDULER.lock() = Some(SchedulerState {
        tasks: alloc::vec![boot_task],
        ready_queue: VecDeque::new(),
        current: 0,
        last_accounted_tick: 0,
    });
}

/// Spawns a new kernel task with its own guarded stack, ready to run.
pub fn spawn(entry: extern "C" fn() -> !) -> TaskId {
    let stack_top = alloc_task_stack();
    let saved_rsp = prepare_initial_stack(stack_top, entry);

    let mut guard = SCHEDULER.lock();
    let s = guard.as_mut().expect("scheduler not initialized");
    let id = s.tasks.len() as TaskId;
    s.tasks.push(Tcb {
        id,
        saved_rsp,
        state: TaskState::Ready,
        switches: 0,
        ticks_run: 0,
    });
    s.ready_queue.push_back(s.tasks.len() - 1);
    id
}

/// Called from the timer interrupt (or voluntarily) to potentially switch
/// to the next ready task. If nothing else is ready, returns immediately
/// without switching.
pub fn schedule() {
    // try_lock, not lock: this runs inside the timer ISR. If the interrupted
    // task was holding the scheduler lock (spawn, task_stats, ...), spinning
    // here would deadlock the whole single-core machine -- the holder can
    // never run again to release it. Skipping one reschedule opportunity and
    // retrying next slice is always safe.
    let Some(mut guard) = SCHEDULER.try_lock() else {
        return;
    };
    let s = guard.as_mut().expect("scheduler not initialized");

    // Credit the outgoing task with the ticks that actually elapsed while it
    // held the CPU, whether or not anything else is ready to take over.
    // Charging a fixed TIME_SLICE_TICKS here instead would overcount: a
    // reschedule that a failed try_lock skipped, or one that ran with
    // interrupts disabled for a while, does not correspond to exactly one
    // slice -- and the per-task totals then sum to more than the machine's
    // uptime, which makes the reported cpu% meaningless.
    let current = s.current;
    let now = super::timer::ticks();
    s.tasks[current].ticks_run += now.saturating_sub(s.last_accounted_tick);
    s.last_accounted_tick = now;

    let Some(next) = s.ready_queue.pop_front() else {
        return; // nothing else is ready
    };
    if next == s.current {
        s.ready_queue.push_back(next);
        return;
    }

    s.tasks[current].state = TaskState::Ready;
    s.ready_queue.push_back(current);
    s.tasks[next].state = TaskState::Running;
    s.tasks[next].switches += 1;
    s.current = next;

    let prev_rsp_out = &mut s.tasks[current].saved_rsp as *mut u64;
    let next_rsp = s.tasks[next].saved_rsp;

    // Must release the lock before switching: this call doesn't return
    // until *this* task (`current`) is switched back into, by which point
    // some other invocation of `schedule` will need this same lock.
    drop(guard);

    unsafe {
        switch_to(prev_rsp_out, next_rsp);
    }
}

pub fn task_count() -> usize {
    SCHEDULER
        .lock()
        .as_ref()
        .map(|s| s.tasks.len())
        .unwrap_or(0)
}

/// A snapshot of one task's scheduling counters, for the `tasks` shell
/// command and the Stats window.
#[derive(Clone, Copy)]
pub struct TaskStat {
    pub id: TaskId,
    pub state: TaskState,
    pub switches: u64,
    pub ticks_run: u64,
}

pub fn task_stats() -> Vec<TaskStat> {
    // Interrupts off while holding the lock would also work, but try-locking
    // in schedule() already makes ISR-vs-task contention safe; a plain lock
    // here just briefly delays one reschedule at worst.
    SCHEDULER
        .lock()
        .as_ref()
        .map(|s| {
            s.tasks
                .iter()
                .map(|t| TaskStat {
                    id: t.id,
                    state: t.state,
                    switches: t.switches,
                    ticks_run: t.ticks_run,
                })
                .collect()
        })
        .unwrap_or_default()
}
