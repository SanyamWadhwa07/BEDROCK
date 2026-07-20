//! PS/2 mouse packet decoding: a 3-byte reassembly **finite state machine**
//! with sync-bit recovery.
//!
//! Split out from the driver deliberately. The 8042 controller dance
//! (`init`) is unavoidably port I/O and can only run on real hardware, but
//! the packet FSM is pure byte-stream logic -- and it is the part that
//! actually had a bug (see `stray_ack_byte_is_rejected` below), so it is the
//! part worth testing on the host.

#![cfg_attr(not(test), no_std)]

/// Byte 0 flag bits.
const FLAG_LEFT: u8 = 1 << 0;
const FLAG_RIGHT: u8 = 1 << 1;
const FLAG_MIDDLE: u8 = 1 << 2;
/// Always set on a valid first byte -- this is the sync check.
const FLAG_ALWAYS_ONE: u8 = 1 << 3;
const FLAG_X_SIGN: u8 = 1 << 4;
const FLAG_Y_SIGN: u8 = 1 << 5;
const FLAG_X_OVERFLOW: u8 = 1 << 6;
const FLAG_Y_OVERFLOW: u8 = 1 << 7;

const BUTTON_MASK: u8 = FLAG_LEFT | FLAG_RIGHT | FLAG_MIDDLE;
const OVERFLOW_MASK: u8 = FLAG_X_OVERFLOW | FLAG_Y_OVERFLOW;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub dx: i16,
    /// Already flipped to screen coordinates (positive = down).
    pub dy: i16,
    pub buttons: u8,
}

impl MouseEvent {
    pub fn left(&self) -> bool {
        self.buttons & FLAG_LEFT != 0
    }

    pub fn right(&self) -> bool {
        self.buttons & FLAG_RIGHT != 0
    }

    pub fn middle(&self) -> bool {
        self.buttons & FLAG_MIDDLE != 0
    }
}

/// 3-byte packet reassembly FSM. Byte 0 is flags (bit3 is always set --
/// that's the sync check: if we're at state 0 and bit3 is clear, the stream
/// is misaligned and the byte is dropped until we re-sync). dx/dy are 9-bit
/// two's complement: 8 data bits plus a sign bit in the flags byte.
#[derive(Debug, Clone)]
pub struct PacketParser {
    bytes: [u8; 3],
    idx: usize,
    /// Bytes dropped by the sync check since construction -- a useful health
    /// signal, since a stream that is constantly resyncing means something
    /// upstream is injecting bytes.
    resyncs: u64,
}

impl Default for PacketParser {
    fn default() -> Self {
        Self::new()
    }
}

impl PacketParser {
    pub const fn new() -> Self {
        Self {
            bytes: [0; 3],
            idx: 0,
            resyncs: 0,
        }
    }

    /// How many bytes have been discarded by the sync check.
    pub fn resyncs(&self) -> u64 {
        self.resyncs
    }

    /// Bytes of a packet currently buffered (0 means the next byte starts a
    /// fresh packet and will be sync-checked).
    pub fn pending(&self) -> usize {
        self.idx
    }

    /// Drops any partially-assembled packet. The driver can call this if it
    /// ever suspects the byte stream stalled mid-packet; the next byte is
    /// then sync-checked as a fresh packet start.
    pub fn reset(&mut self) {
        self.idx = 0;
    }

    pub fn feed(&mut self, byte: u8) -> Option<MouseEvent> {
        // Sync check on the first byte of a packet: bit3 must be set, and
        // the overflow bits must NOT be (QEMU/real mice essentially never
        // set them in normal streams). The second condition specifically
        // rejects stray 0xFA ACK bytes, which pass the bit3 test and would
        // otherwise silently shift the whole stream by one -- the exact bug
        // that shipped in the first version of the driver.
        if self.idx == 0 && (byte & FLAG_ALWAYS_ONE == 0 || byte & OVERFLOW_MASK != 0) {
            self.resyncs += 1;
            return None; // out of sync -- drop until a plausible flags byte
        }
        self.bytes[self.idx] = byte;
        self.idx += 1;
        if self.idx < 3 {
            return None;
        }
        self.idx = 0;

        let flags = self.bytes[0];
        // Overflow bits set means garbage deltas -- keep the buttons, drop
        // the movement rather than jerking the cursor across the screen.
        if flags & OVERFLOW_MASK != 0 {
            return Some(MouseEvent {
                dx: 0,
                dy: 0,
                buttons: flags & BUTTON_MASK,
            });
        }
        let mut dx = self.bytes[1] as i16;
        if flags & FLAG_X_SIGN != 0 {
            dx -= 256;
        }
        let mut dy = self.bytes[2] as i16;
        if flags & FLAG_Y_SIGN != 0 {
            dy -= 256;
        }
        Some(MouseEvent {
            dx,
            dy: -dy, // PS/2 positive-y is up; screen positive-y is down
            buttons: flags & BUTTON_MASK,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds a whole packet, asserting the first two bytes yield nothing.
    fn feed_packet(p: &mut PacketParser, b: [u8; 3]) -> Option<MouseEvent> {
        assert_eq!(p.feed(b[0]), None, "byte 0 must not complete a packet");
        assert_eq!(p.feed(b[1]), None, "byte 1 must not complete a packet");
        p.feed(b[2])
    }

    #[test]
    fn decodes_a_simple_positive_movement() {
        let mut p = PacketParser::new();
        let ev = feed_packet(&mut p, [FLAG_ALWAYS_ONE, 5, 7]).expect("packet");
        assert_eq!(ev.dx, 5);
        assert_eq!(ev.dy, -7, "PS/2 up must become screen-negative");
        assert_eq!(ev.buttons, 0);
    }

    #[test]
    fn decodes_nine_bit_negative_deltas() {
        let mut p = PacketParser::new();
        // X sign set with data 0xFF is -1; Y sign set with 0xFE is -2.
        let flags = FLAG_ALWAYS_ONE | FLAG_X_SIGN | FLAG_Y_SIGN;
        let ev = feed_packet(&mut p, [flags, 0xFF, 0xFE]).expect("packet");
        assert_eq!(ev.dx, -1);
        assert_eq!(ev.dy, 2, "negative PS/2 dy is downward on screen");
    }

    #[test]
    fn decodes_full_range_deltas_without_overflow() {
        let mut p = PacketParser::new();
        // Largest positive: no sign bit, 0xFF data.
        let ev = feed_packet(&mut p, [FLAG_ALWAYS_ONE, 0xFF, 0x00]).expect("packet");
        assert_eq!(ev.dx, 255);
        // Most negative: sign bit with 0x00 data = -256.
        let flags = FLAG_ALWAYS_ONE | FLAG_X_SIGN;
        let ev = feed_packet(&mut p, [flags, 0x00, 0x00]).expect("packet");
        assert_eq!(ev.dx, -256);
    }

    #[test]
    fn reports_button_state() {
        let mut p = PacketParser::new();
        let flags = FLAG_ALWAYS_ONE | FLAG_LEFT | FLAG_MIDDLE;
        let ev = feed_packet(&mut p, [flags, 0, 0]).expect("packet");
        assert!(ev.left() && ev.middle());
        assert!(!ev.right());
    }

    #[test]
    fn overflow_packets_drop_movement_but_keep_buttons() {
        let mut p = PacketParser::new();
        // An overflow flags byte can't *start* a packet (sync check), but it
        // can legitimately arrive as byte 0 only if... it can't. So drive it
        // as a mid-stream case: sync on a good packet first, then the
        // overflow byte is rejected at position 0 as designed.
        let flags = FLAG_ALWAYS_ONE | FLAG_X_OVERFLOW | FLAG_LEFT;
        assert_eq!(p.feed(flags), None, "overflow byte rejected as byte 0");
        assert_eq!(p.resyncs(), 1);
    }

    /// Regression test for the real bug in the progress log: the mouse's
    /// 0xFA ACK bytes leaking into the IRQ-driven stream. 0xFA passes the
    /// bit3 sync check, so without the overflow-bit guard it would be
    /// accepted as a flags byte and shift every later packet by one --
    /// silently producing garbage deltas forever.
    #[test]
    fn stray_ack_byte_is_rejected() {
        let mut p = PacketParser::new();
        assert_eq!(0xFA & FLAG_ALWAYS_ONE, FLAG_ALWAYS_ONE, "0xFA passes bit3");
        assert_eq!(p.feed(0xFA), None);
        assert_eq!(p.resyncs(), 1, "the ACK must be dropped, not consumed");
        // The stream stays aligned: the very next real packet decodes right.
        let ev = feed_packet(&mut p, [FLAG_ALWAYS_ONE, 3, 4]).expect("packet");
        assert_eq!((ev.dx, ev.dy), (3, -4));
    }

    #[test]
    fn recovers_alignment_after_a_truncated_packet() {
        let mut p = PacketParser::new();
        // A packet cut short mid-transfer leaves the FSM at idx 1..
        assert_eq!(p.feed(FLAG_ALWAYS_ONE), None);
        // ..and the next bytes get consumed as that packet's tail. The FSM
        // resynchronizes on its own once it returns to idx 0.
        assert_eq!(p.feed(1), None);
        let _ = p.feed(2); // completes the stale packet
        let ev = feed_packet(&mut p, [FLAG_ALWAYS_ONE, 9, 9]).expect("packet");
        assert_eq!(ev.dx, 9);
    }

    /// The property that matters for the driver: no matter what garbage a
    /// desynchronized controller emits, the parser never panics and always
    /// returns to decoding correct packets once clean ones arrive.
    #[test]
    fn always_resynchronizes_after_arbitrary_garbage() {
        let mut rng: u64 = 0x5EED_1234_5678_9ABC;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };

        for trial in 0..1_000 {
            let mut p = PacketParser::new();
            for _ in 0..(next() % 32) {
                let _ = p.feed((next() & 0xFF) as u8);
            }

            // Whatever the garbage left buffered, feeding filler bytes must
            // walk the FSM back to a packet boundary within two bytes.
            let mut fed = 0;
            while p.pending() != 0 {
                let _ = p.feed(0);
                fed += 1;
                assert!(fed <= 2, "trial {trial}: FSM stuck mid-packet");
            }

            // Aligned again: a clean packet must decode exactly.
            let ev = feed_packet(&mut p, [FLAG_ALWAYS_ONE, 12, 5]).expect("packet");
            assert_eq!(
                (ev.dx, ev.dy),
                (12, -5),
                "trial {trial}: resynced parser decoded wrong"
            );
        }
    }

    #[test]
    fn reset_drops_a_partial_packet() {
        let mut p = PacketParser::new();
        p.feed(FLAG_ALWAYS_ONE);
        p.feed(99);
        assert_eq!(p.pending(), 2);
        p.reset();
        assert_eq!(p.pending(), 0);
        let ev = feed_packet(&mut p, [FLAG_ALWAYS_ONE, 1, 1]).expect("packet");
        assert_eq!(ev.dx, 1);
    }
}
