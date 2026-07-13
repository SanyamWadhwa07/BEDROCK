use crate::interrupts::PICS;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;
use x86_64::structures::idt::InterruptStackFrame;

const PIT_BASE_FREQUENCY_HZ: u32 = 1_193_182;
const PIT_TICK_HZ: u32 = 100;
/// Switch tasks roughly every 5 ticks at 100Hz (~50ms time slices) -- coarse
/// enough to see clean interleaving in serial output, fine enough to feel
/// responsive.
const TIME_SLICE_TICKS: u64 = 5;

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

pub extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let tick = TICKS.fetch_add(1, Ordering::Relaxed) + 1;

    // Acknowledge with the PIC *before* possibly switching tasks -- if we
    // switched first, this exact handler invocation might not resume for a
    // while, and the PIC would withhold further interrupts until EOI'd.
    unsafe {
        PICS.lock().notify_end_of_interrupt(TIMER_INTERRUPT_VECTOR);
    }

    if tick % TIME_SLICE_TICKS == 0 {
        super::scheduler::schedule();
    }
}
