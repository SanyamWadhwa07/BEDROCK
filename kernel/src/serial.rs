use spin::{Lazy, Mutex};
use uart_16550::SerialPort;
use x86_64::instructions::port::Port;

const COM1_BASE: u16 = 0x3F8;
const COM1_LSR: u16 = COM1_BASE + 5;
const LSR_DATA_READY: u8 = 1;

pub static SERIAL1: Lazy<Mutex<SerialPort>> = Lazy::new(|| {
    let mut port = unsafe { SerialPort::new(COM1_BASE) };
    port.init();
    Mutex::new(port)
});

/// Polls the UART's Line Status Register directly, *without* touching
/// `SERIAL1`'s lock. Callers should only call `SERIAL1.lock().receive()`
/// (which blocks internally until a byte arrives) once this returns true --
/// otherwise, in a preemptive-multitasking kernel, the lock would sit held
/// for the entire blocking wait, starving every other task's `serial_print!`.
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
