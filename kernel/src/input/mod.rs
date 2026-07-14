pub mod event_queue;
pub mod mouse;

use event_queue::ByteRing;
use x86_64::instructions::port::Port;

/// Raw scancode/data streams, filled by the ISRs, drained in task context
/// (the render task) where the actual decoding happens -- keeps the
/// interrupt handlers down to "read one port, push one byte, EOI".
pub static KEYBOARD_QUEUE: ByteRing<256> = ByteRing::new();
pub static MOUSE_QUEUE: ByteRing<256> = ByteRing::new();

/// Called from the IRQ1/IRQ12 handlers.
pub fn isr_read_data_port() -> u8 {
    let mut p: Port<u8> = Port::new(0x60);
    unsafe { p.read() }
}

/// One-time device bring-up; must run before `sti` (see mouse::init docs).
pub fn init_devices() {
    if mouse::init() {
        crate::serial_println!("PS/2 mouse enabled (streaming, IRQ12).");
    } else {
        crate::serial_println!("WARNING: PS/2 mouse init failed; continuing without mouse.");
    }
}
