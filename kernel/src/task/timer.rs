use crate::interrupts::PICS;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;
use x86_64::structures::idt::InterruptStackFrame;

const PIT_BASE_FREQUENCY_HZ: u32 = 1_193_182;
const PIT_TICK_HZ: u32 = 100;
/// Switch tasks roughly every 5 ticks at 100Hz (~50ms time slices) -- coarse
/// enough to see clean interleaving in serial output, fine enough to feel
/// responsive.
pub(crate) const TIME_SLICE_TICKS: u64 = 5;

pub const TIMER_INTERRUPT_VECTOR: u8 = crate::interrupts::PIC_1_OFFSET; // IRQ0

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Programs PIT channel 0 (the legacy 8253/8254 timer) for a ~100Hz square
/// wave via the command port, then loads the 16-bit reload divisor lobyte/hibyte.
pub fn init_pit() {
    let divisor = (PIT_BASE_FREQUENCY_HZ / PIT_TICK_HZ) as u16;
    unsafe {
        let mut command: Port<u8> = Port::new(0x43);
        let mut channel0: Port<u8> = Port::new(0x40);
        command.write(0x36u8); // channel 0, lobyte/hibyte access, mode 3 (square wave)
        channel0.write((divisor & 0xff) as u8);
        channel0.write((divisor >> 8) as u8);
    }
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn ticks_to_ms(ticks: u64) -> u64 {
    ticks * 1000 / PIT_TICK_HZ as u64
}

pub fn uptime_ms() -> u64 {
    ticks_to_ms(ticks())
}

/// Reads the CPU's time-stamp counter -- the only sub-tick clock this
/// kernel has. Raw cycles are meaningless on their own; convert through
/// [`cycles_to_us`]/[`cycles_to_ns`] after [`calibrate_tsc`] has run.
pub fn rdtsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// TSC cycles per millisecond, measured against the PIT at boot.
/// Zero until `calibrate_tsc` runs.
static TSC_PER_MS: AtomicU64 = AtomicU64::new(0);

pub fn tsc_per_ms() -> u64 {
    TSC_PER_MS.load(Ordering::Relaxed)
}

pub fn cycles_to_us(cycles: u64) -> u64 {
    let per_ms = tsc_per_ms().max(1);
    ((cycles as u128 * 1000) / per_ms as u128) as u64
}

pub fn cycles_to_ns(cycles: u64) -> u64 {
    let per_ms = tsc_per_ms().max(1);
    ((cycles as u128 * 1_000_000) / per_ms as u128) as u64
}

/// Captures a `(tick, tsc)` pair immediately after a fresh tick edge. The
/// pair itself is read with interrupts disabled so it can never straddle a
/// tick increment or a preemption -- worst case the pair lands somewhere
/// *inside* a tick instead of exactly on its edge, which the median over
/// several samples in `calibrate_tsc` absorbs.
fn edge_pair() -> (u64, u64) {
    let start = ticks();
    while ticks() == start {
        core::hint::spin_loop();
    }
    x86_64::instructions::interrupts::without_interrupts(|| (ticks(), rdtsc()))
}

/// One-time TSC-vs-PIT calibration: five samples of ten ticks each
/// (~0.6s total), median taken so a sample polluted by an unlucky
/// preemption between the tick edge and the TSC read can't skew the
/// result. Must run with interrupts enabled and the PIT armed.
pub fn calibrate_tsc() {
    const SAMPLE_TICKS: u64 = 10;
    const SAMPLES: usize = 5;

    let mut per_tick = [0u64; SAMPLES];
    for sample in per_tick.iter_mut() {
        let (t0, c0) = edge_pair();
        while ticks() < t0 + SAMPLE_TICKS {
            core::hint::spin_loop();
        }
        let (t1, c1) = edge_pair();
        *sample = (c1 - c0) / (t1 - t0);
    }
    per_tick.sort_unstable();
    let cycles_per_tick = per_tick[SAMPLES / 2];
    let per_ms = cycles_per_tick * PIT_TICK_HZ as u64 / 1000;
    TSC_PER_MS.store(per_ms, Ordering::Relaxed);
    crate::serial_println!(
        "TSC calibrated against PIT: ~{} cycles/ms (~{} MHz effective).",
        per_ms,
        per_ms / 1000
    );
}

pub extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let tick = TICKS.fetch_add(1, Ordering::Relaxed) + 1;

    // Acknowledge with the PIC *before* possibly switching tasks -- if we
    // switched first, this exact handler invocation might not resume for a
    // while, and the PIC would withhold further interrupts until EOI'd.
    unsafe {
        PICS.lock().notify_end_of_interrupt(TIMER_INTERRUPT_VECTOR);
    }

    if tick.is_multiple_of(TIME_SLICE_TICKS) {
        super::scheduler::schedule();
    }
}
