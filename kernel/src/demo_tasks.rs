//! Background kernel threads for the live demo. As of Phase 4 they no longer
//! print to serial -- they just increment counters that the render task
//! displays on screen, proving the preemptive scheduler and the graphics
//! stack are alive *simultaneously* without fighting over the serial lock
//! (see the Phase-3 lock-convoy war story in PLAN.md).

use core::hint::spin_loop;
use core::sync::atomic::{AtomicU64, Ordering};

static COUNT_PING: AtomicU64 = AtomicU64::new(0);
static COUNT_PONG: AtomicU64 = AtomicU64::new(0);
static COUNT_C: AtomicU64 = AtomicU64::new(0);
static COUNT_D: AtomicU64 = AtomicU64::new(0);
static COUNT_E: AtomicU64 = AtomicU64::new(0);

pub fn snapshot() -> [(&'static str, u64); 5] {
    [
        ("ping", COUNT_PING.load(Ordering::Relaxed)),
        ("pong", COUNT_PONG.load(Ordering::Relaxed)),
        ("C", COUNT_C.load(Ordering::Relaxed)),
        ("D", COUNT_D.load(Ordering::Relaxed)),
        ("E", COUNT_E.load(Ordering::Relaxed)),
    ]
}

fn count_forever(counter: &AtomicU64, delay_iters: u64) -> ! {
    loop {
        counter.fetch_add(1, Ordering::Relaxed);
        for _ in 0..delay_iters {
            spin_loop();
        }
    }
}

pub extern "C" fn task_ping() -> ! {
    count_forever(&COUNT_PING, 20_000)
}

pub extern "C" fn task_pong() -> ! {
    count_forever(&COUNT_PONG, 20_000)
}

pub extern "C" fn task_c() -> ! {
    // The "hostile" fast task: shortest delay of all. (Zero-delay caused the
    // Phase-3 serial lock-convoy; with silent counters zero would be safe
    // again, but a small delay keeps the counter human-readable.)
    count_forever(&COUNT_C, 2_000)
}

pub extern "C" fn task_d() -> ! {
    count_forever(&COUNT_D, 20_000)
}

pub extern "C" fn task_e() -> ! {
    count_forever(&COUNT_E, 20_000)
}
