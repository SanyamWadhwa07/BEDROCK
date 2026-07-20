use crate::input::event_queue::ByteRing;
use spin::{Lazy, Mutex};
use uart_16550::SerialPort;
use x86_64::instructions::port::Port;

const COM1_BASE: u16 = 0x3F8;
const COM1_IER: u16 = COM1_BASE + 1; // interrupt enable register
const COM1_FCR: u16 = COM1_BASE + 2; // FIFO control register (write-only)
const COM1_LSR: u16 = COM1_BASE + 5; // line status register
const LSR_DATA_READY: u8 = 1;
const IER_RECEIVED_DATA: u8 = 1;

/// FIFO enable + clear both FIFOs, leaving the receive trigger level at its
/// lowest setting (bits 6-7 clear = one byte).
const FCR_ENABLE_CLEAR_TRIGGER_1: u8 = 0b0000_0111;

pub static SERIAL1: Lazy<Mutex<SerialPort>> = Lazy::new(|| {
    let mut port = unsafe { SerialPort::new(COM1_BASE) };
    port.init();
    Mutex::new(port)
});

/// Received bytes, filled by the IRQ4 handler and drained by the shell.
///
/// This exists because polling the UART from task context genuinely loses
/// input: the shell is one of seven round-robin tasks, so it is descheduled
/// for ~6/7 of every rotation, and the 16550's one-byte receive register is
/// overwritten by the next arriving byte long before the shell is scheduled
/// again. Pasting or scripting a command into the serial console dropped
/// most of its characters. Moving reception into the ISR -- which runs
/// regardless of which task holds the CPU -- is the actual fix; the ring
/// buffer then absorbs the burst.
pub static SERIAL_QUEUE: ByteRing<256> = ByteRing::new();

/// Enables the UART's "received data available" interrupt (IRQ4), and drops
/// the receive FIFO trigger level to a single byte. Called once at boot,
/// before `sti`.
///
/// The trigger level is not incidental. `uart_16550`'s `init()` leaves the
/// FIFO trigger at 14 bytes, which means the UART only raises IRQ4 once 14
/// bytes have piled up. A shorter burst, or the tail of a longer one, sits
/// in the FIFO below the threshold and raises nothing. The symptom was a
/// command typed as `bench ctxswitch\n` arriving as exactly `bench ctxswitc`:
/// 14 bytes delivered, the last two stranded, and the newline that would
/// have submitted the line among them. The 16550 does define a character
/// timeout interrupt for exactly this case, but relying on it means trusting
/// an emulated corner; a one-byte trigger is unambiguous, and the handler
/// drains in a loop anyway, so the FIFO still does its job under load.
pub fn enable_receive_interrupt() {
    let mut fcr: Port<u8> = Port::new(COM1_FCR);
    unsafe { fcr.write(FCR_ENABLE_CLEAR_TRIGGER_1) };
    let mut ier: Port<u8> = Port::new(COM1_IER);
    unsafe { ier.write(IER_RECEIVED_DATA) };
}

/// Reads one byte straight from the UART data port. ISR-only: it must not
/// touch `SERIAL1`'s lock, which the interrupted task may be holding
/// mid-`serial_print!`.
pub fn isr_read_byte() -> u8 {
    let mut data: Port<u8> = Port::new(COM1_BASE);
    unsafe { data.read() }
}

/// True while the UART still has a received byte waiting. The ISR drains in
/// a loop on this, since one interrupt can cover several buffered bytes.
pub fn byte_ready() -> bool {
    let mut lsr: Port<u8> = Port::new(COM1_LSR);
    unsafe { (lsr.read() & LSR_DATA_READY) != 0 }
}

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    use core::fmt::Write;
    SERIAL1
        .lock()
        .write_fmt(args)
        .expect("write to serial port failed");
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => (
        $crate::serial_print!(concat!($fmt, "\n"), $($arg)*)
    );
}
