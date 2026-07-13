use super::context_switch::switch_to;
use super::tcb::{alloc_task_stack, prepare_initial_stack, Tcb, TaskId, TaskState};
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
    };
    *SCHEDULER.lock() = Some(SchedulerState {
        tasks: alloc::vec![boot_task],
        ready_queue: VecDeque::new(),
        current: 0,
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
    });
    s.ready_queue.push_back(s.tasks.len() - 1);
    id
}

/// Called from the timer interrupt (or voluntarily) to potentially switch
/// to the next ready task. If nothing else is ready, returns immediately
/// without switching.
pub fn schedule() {
    let mut guard = SCHEDULER.lock();
    let s = guard.as_mut().expect("scheduler not initialized");

    let Some(next) = s.ready_queue.pop_front() else {
        return; // nothing else is ready
    };
    if next == s.current {
        s.ready_queue.push_back(next);
        return;
    }

    let current = s.current;
    s.tasks[current].state = TaskState::Ready;
    s.ready_queue.push_back(current);
    s.tasks[next].state = TaskState::Running;
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

pub fn task_ids() -> Vec<TaskId> {
    SCHEDULER
        .lock()
        .as_ref()
        .map(|s| s.tasks.iter().map(|t| t.id).collect())
        .unwrap_or_default()
}
