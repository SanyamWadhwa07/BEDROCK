//! PS/2 mouse: the hand-rolled part of the input stack. This module owns the
//! classic 8042-controller auxiliary-device enable dance -- unavoidably port
//! I/O, so it can only run on real hardware.
//!
//! The 3-byte packet reassembly FSM it feeds is pure byte-stream logic and
//! lives in the `bedrock-input` crate instead, where ordinary `cargo test`
//! covers it on the host (including a regression test for the stray-0xFA-ACK
//! desync bug documented in `init` below). Re-exported here so the WM keeps
//! using `input::mouse::{PacketParser, MouseEvent}`.

// `MouseEvent` is named through `PacketParser::feed`'s return type rather
// than directly, but stays re-exported as part of this module's surface.
#[allow(unused_imports)]
pub use bedrock_input::{MouseEvent, PacketParser};

use x86_64::instructions::port::Port;

const STATUS_PORT: u16 = 0x64; // read: status, write: command
const DATA_PORT: u16 = 0x60;

const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;

fn status() -> u8 {
    let mut p: Port<u8> = Port::new(STATUS_PORT);
    unsafe { p.read() }
}

fn wait_can_write() -> bool {
    for _ in 0..100_000 {
        if status() & STATUS_INPUT_FULL == 0 {
            return true;
        }
    }
    false
}

fn wait_can_read() -> bool {
    for _ in 0..100_000 {
        if status() & STATUS_OUTPUT_FULL != 0 {
            return true;
        }
    }
    false
}

fn write_command(cmd: u8) -> bool {
    if !wait_can_write() {
        return false;
    }
    let mut p: Port<u8> = Port::new(STATUS_PORT);
    unsafe { p.write(cmd) };
    true
}

fn write_data(b: u8) -> bool {
    if !wait_can_write() {
        return false;
    }
    let mut p: Port<u8> = Port::new(DATA_PORT);
    unsafe { p.write(b) };
    true
}

fn read_data() -> Option<u8> {
    if !wait_can_read() {
        return None;
    }
    let mut p: Port<u8> = Port::new(DATA_PORT);
    Some(unsafe { p.read() })
}

/// Sends one byte to the mouse itself (0xD4 prefix routes the next data
/// byte to the auxiliary device) and consumes the 0xFA ACK.
fn send_to_mouse(b: u8) -> bool {
    if !write_command(0xD4) || !write_data(b) {
        return false;
    }
    matches!(read_data(), Some(0xFA))
}

/// The standard 8042 aux-enable sequence. Must run with hardware
/// interrupts still disabled (before `sti`): the ACK bytes are consumed by
/// polling here, and a live IRQ12 handler would race us for them.
///
/// Ordering matters (found the hard way): the config-byte IRQ12 enable must
/// come LAST. In the first version it came first, so the 0xFA ACKs for
/// 0xF6/0xF4 raised a pending IRQ12; after `sti` the handler fired once and
/// pushed a stale 0xFA into the data stream, permanently shifting every
/// subsequent 3-byte packet by one (0xFA even passes the bit3 sync check).
pub fn init() -> bool {
    // Enable the auxiliary (mouse) port.
    if !write_command(0xA8) {
        return false;
    }
    // Mouse defaults + enable streaming FIRST, while IRQ12 is still
    // disabled in the controller config -- their ACKs get polled away
    // here and can't leave a pending interrupt behind.
    if !send_to_mouse(0xF6) {
        return false;
    }
    if !send_to_mouse(0xF4) {
        return false;
    }
    // Now read controller config byte, set bit1 (IRQ12 enable), clear bit5
    // (mouse clock disable), write it back.
    if !write_command(0x20) {
        return false;
    }
    let Some(mut config) = read_data() else {
        return false;
    };
    config |= 0b0000_0010;
    config &= !0b0010_0000;
    if !write_command(0x60) || !write_data(config) {
        return false;
    }
    // Belt and braces: drain anything still sitting in the output buffer so
    // the IRQ-driven stream starts clean and packet-aligned.
    let mut p: Port<u8> = Port::new(DATA_PORT);
    for _ in 0..16 {
        if status() & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        let _ = unsafe { p.read() };
    }
    true
}

// `MouseEvent` and `PacketParser` are re-exported from `bedrock-input` at
// the top of this module.
