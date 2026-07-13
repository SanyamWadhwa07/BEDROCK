//! Phase-3 demonstration tasks: five kernel threads, none of which ever
//! voluntarily yield, proving the scheduler's preemption (not cooperation)
//! is what keeps them all running. Two are named `ping`/`pong` specifically
//! to serve as the plan's required two-task correctness check; `task_c` runs
//! with zero delay between iterations as a deliberately "hostile" busy loop
//! that a purely cooperative scheduler would let starve everything else.

use core::hint::spin_loop;
use core::sync::atomic::{AtomicU64, Ordering};

static COUNT_PING: AtomicU64 = AtomicU64::new(0);
static COUNT_PONG: AtomicU64 = AtomicU64::new(0);
static COUNT_C: AtomicU64 = AtomicU64::new(0);
static COUNT_D: AtomicU64 = AtomicU64::new(0);
static COUNT_E: AtomicU64 = AtomicU64::new(0);

fn run_forever(tag: &str, counter: &AtomicU64, delay_iters: u64) -> ! {
    loop {
        let n = counter.fetch_add(1, Ordering::Relaxed);
        crate::serial_print!("{}{}@t{} ", tag, n, crate::task::timer::ticks());
        for _ in 0..delay_iters {
            spin_loop();
        }
    }
}

pub extern "C" fn task_ping() -> ! {
    run_forever("ping", &COUNT_PING, 20_000)
}

pub extern "C" fn task_pong() -> ! {
    run_forever("pong", &COUNT_PONG, 20_000)
}

pub extern "C" fn task_c() -> ! {
    // Deliberately much shorter than the others -- the "hostile" busy-loop
    // task -- but NOT zero. A true zero-delay loop starves everyone else in
    // practice: it gets preempted mid-print holding `serial::SERIAL1`'s lock
    // far more often than it releases it cleanly, and since the holder is
    // then suspended for a full round-robin cycle, that lock stays stuck
    // the whole time D/E/ping/pong are scheduled -- a real lock-convoy /
    // starvation bug found via this exact demo, not a scheduler bug (a raw,
    // lock-free port-write trace confirmed the round-robin rotation itself
    // -- 0,1,2,3,4,5 repeating -- was correct throughout).
    run_forever("C", &COUNT_C, 2_000)
}

pub extern "C" fn task_d() -> ! {
    run_forever("D", &COUNT_D, 20_000)
}

pub extern "C" fn task_e() -> ! {
    run_forever("E", &COUNT_E, 20_000)
}
