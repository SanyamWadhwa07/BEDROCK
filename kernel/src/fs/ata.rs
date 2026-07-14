//! ATA PIO driver: legacy fixed I/O ports (0x1F0-0x1F7), the primary
//! channel's *slave* device (0xF0 select) so the boot disk on the primary
//! master is never touched. No PCI enumeration, no DMA -- one `in`/`out`
//! per register, polled status, 28-bit LBA. Simple and slow, which is the
//! deliberate scope cut named in the plan (ATA PIO vs. AHCI).

use x86_64::instructions::port::Port;

const DATA: u16 = 0x1F0;
const ERROR: u16 = 0x1F1;
const SECTOR_COUNT: u16 = 0x1F2;
const LBA_LOW: u16 = 0x1F3;
const LBA_MID: u16 = 0x1F4;
const LBA_HIGH: u16 = 0x1F5;
const DRIVE_HEAD: u16 = 0x1F6;
const STATUS: u16 = 0x1F7;
const COMMAND: u16 = 0x1F7;
const ALT_STATUS: u16 = 0x3F6;

const CMD_READ_SECTORS: u8 = 0x20;
const CMD_WRITE_SECTORS: u8 = 0x30;
const CMD_FLUSH_CACHE: u8 = 0xE7;

const STATUS_ERR: u8 = 1 << 0;
const STATUS_DRQ: u8 = 1 << 3;
const STATUS_BSY: u8 = 1 << 7;

/// Device Control Register bit 1: "no interrupt enable" -- setting it
/// stops the controller from asserting IRQ14 at all. This driver is
/// polling-only (that's the whole point of PIO here), so an unclaimed
/// legacy IRQ14 has no handler installed for it; leaving nIEN clear (the
/// power-on default) caused a real double fault in testing the moment
/// interrupts were globally enabled, since the very next completed ATA
/// transfer's interrupt had nowhere valid to go. See PLAN.md's Phase 7 log.
const CTRL_NIEN: u8 = 1 << 1;

pub const SECTOR_SIZE: usize = 512;

/// Disables the controller's own IRQ14 generation. Must run once before
/// any transfer -- called from `simplefs::init()`, which itself must run
/// before interrupts are globally enabled.
pub fn disable_interrupts() {
    let mut p: Port<u8> = Port::new(ALT_STATUS);
    unsafe {
        p.write(CTRL_NIEN);
    }
}

fn status() -> u8 {
    let mut p: Port<u8> = Port::new(STATUS);
    unsafe { p.read() }
}

/// Four dummy reads of the alternate status register, the standard ~400ns
/// settle delay real ATA controllers need after a drive-select change.
fn io_wait() {
    let mut p: Port<u8> = Port::new(ALT_STATUS);
    for _ in 0..4 {
        unsafe {
            p.read();
        }
    }
}

fn wait_not_busy() {
    while status() & STATUS_BSY != 0 {
        core::hint::spin_loop();
    }
}

/// Waits for the drive to signal it's ready to transfer data (DRQ), or
/// returns the error status if it signalled ERR instead.
fn wait_drq() -> Result<(), u8> {
    loop {
        let s = status();
        if s & STATUS_ERR != 0 {
            let mut err: Port<u8> = Port::new(ERROR);
            return Err(unsafe { err.read() });
        }
        if s & STATUS_DRQ != 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
}

fn select_slave(lba: u32) {
    let mut p: Port<u8> = Port::new(DRIVE_HEAD);
    unsafe {
        p.write(0xF0 | (((lba >> 24) & 0x0F) as u8));
    }
    io_wait();
}

fn setup_lba(lba: u32, sector_count: u8) {
    unsafe {
        let mut count: Port<u8> = Port::new(SECTOR_COUNT);
        let mut low: Port<u8> = Port::new(LBA_LOW);
        let mut mid: Port<u8> = Port::new(LBA_MID);
        let mut high: Port<u8> = Port::new(LBA_HIGH);
        count.write(sector_count);
        low.write((lba & 0xFF) as u8);
        mid.write(((lba >> 8) & 0xFF) as u8);
        high.write(((lba >> 16) & 0xFF) as u8);
    }
}

/// Reads one 512-byte sector at 28-bit LBA `lba` from the primary slave.
pub fn read_sector(lba: u32, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str> {
    select_slave(lba);
    wait_not_busy();
    setup_lba(lba, 1);
    unsafe {
        let mut cmd: Port<u8> = Port::new(COMMAND);
        cmd.write(CMD_READ_SECTORS);
    }
    wait_drq().map_err(|_| "ATA read error")?;

    let mut data: Port<u16> = Port::new(DATA);
    for chunk in buf.chunks_exact_mut(2) {
        let word = unsafe { data.read() };
        chunk[0] = (word & 0xFF) as u8;
        chunk[1] = (word >> 8) as u8;
    }
    Ok(())
}

/// Writes one 512-byte sector at 28-bit LBA `lba` on the primary slave,
/// then flushes the write cache so it's actually durable before returning
/// (load-bearing for the "reboot QEMU, data survives" persistence demo).
pub fn write_sector(lba: u32, buf: &[u8; SECTOR_SIZE]) -> Result<(), &'static str> {
    select_slave(lba);
    wait_not_busy();
    setup_lba(lba, 1);
    unsafe {
        let mut cmd: Port<u8> = Port::new(COMMAND);
        cmd.write(CMD_WRITE_SECTORS);
    }
    wait_drq().map_err(|_| "ATA write error")?;

    let mut data: Port<u16> = Port::new(DATA);
    for chunk in buf.chunks_exact(2) {
        let word = chunk[0] as u16 | ((chunk[1] as u16) << 8);
        unsafe {
            data.write(word);
        }
    }

    wait_not_busy();
    unsafe {
        let mut cmd: Port<u8> = Port::new(COMMAND);
        cmd.write(CMD_FLUSH_CACHE);
    }
    wait_not_busy();
    Ok(())
}
