//! PS/2 mouse: the hand-rolled part of the input stack. Covers the classic
//! 8042-controller auxiliary-device enable dance and a 3-byte packet
//! finite state machine with sync-bit recovery.

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

#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    pub dx: i16,
    pub dy: i16, // already flipped to screen coordinates (positive = down)
    pub buttons: u8,
}

/// 3-byte packet reassembly FSM. Byte 0 is flags (bit3 is always set --
/// that's the sync check: if we're at state 0 and bit3 is clear, the stream
/// is misaligned and the byte is dropped until we re-sync). dx/dy are 9-bit
/// two's complement: 8 data bits plus a sign bit in the flags byte.
pub struct PacketParser {
    bytes: [u8; 3],
    idx: usize,
}

impl PacketParser {
    pub const fn new() -> Self {
        Self { bytes: [0; 3], idx: 0 }
    }

    pub fn feed(&mut self, byte: u8) -> Option<MouseEvent> {
        // Sync check on the first byte of a packet: bit3 must be set, and
        // the overflow bits must NOT be (QEMU/real mice essentially never
        // set them in normal streams). The second condition specifically
        // rejects stray 0xFA ACK bytes, which pass the bit3 test and would
        // otherwise silently shift the whole stream by one.
        if self.idx == 0 && (byte & 0x08 == 0 || byte & 0xC0 != 0) {
            return None; // out of sync -- drop until a plausible flags byte
        }
        self.bytes[self.idx] = byte;
        self.idx += 1;
        if self.idx < 3 {
            return None;
        }
        self.idx = 0;

        let flags = self.bytes[0];
        // Overflow bits set means garbage deltas -- discard the packet.
        if flags & 0b1100_0000 != 0 {
            return Some(MouseEvent { dx: 0, dy: 0, buttons: flags & 0x7 });
        }
        let mut dx = self.bytes[1] as i16;
        if flags & 0x10 != 0 {
            dx -= 256;
        }
        let mut dy = self.bytes[2] as i16;
        if flags & 0x20 != 0 {
            dy -= 256;
        }
        Some(MouseEvent {
            dx,
            dy: -dy, // PS/2 positive-y is up; screen positive-y is down
            buttons: flags & 0x7,
        })
    }
}
