use alloc::vec;
use alloc::vec::Vec;
use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use spin::Mutex;
use x86_64::structures::paging::{FrameAllocator, FrameDeallocator, PhysFrame, Size4KiB};
use x86_64::PhysAddr;

const FRAME_SIZE: u64 = 4096;

/// Stage A: a bump allocator over the bootloader's memory map. No `dealloc`
/// -- used only to build the initial page tables and back the bootstrap
/// heap, before a heap (and therefore `Vec`) exists to back Stage B.
pub struct BumpFrameAllocator {
    memory_map: &'static MemoryRegions,
    next: usize,
}

impl BumpFrameAllocator {
    pub fn init(memory_map: &'static MemoryRegions) -> Self {
        Self {
            memory_map,
            next: 0,
        }
    }

    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> + '_ {
        self.memory_map
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .flat_map(|r| (r.start..r.end).step_by(FRAME_SIZE as usize))
            .map(|addr| PhysFrame::containing_address(PhysAddr::new(addr)))
    }

    /// How many frames handed out so far. Stage B needs this to know which
    /// leading usable frames are already spoken for.
    pub fn frames_allocated(&self) -> usize {
        self.next
    }
}

unsafe impl FrameAllocator<Size4KiB> for BumpFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}

/// Stage B: a bitset allocator (1 bit per 4KiB frame, the classic
/// bit-manipulation DSA structure -- packed into `u8` words, indexed via
/// `idx/8` + `1 << (idx%8)`) covering physical memory from address 0 up to
/// the end of the highest usable/bootloader region. Built from the heap (so
/// it needs the heap to already exist), promoting from `BumpFrameAllocator`
/// by marking every frame Stage A already handed out as used. Allocation
/// scans from a rotating cursor (`next_hint`) rather than restarting at 0
/// each time, avoiding an O(n) walk on every call in the common case.
pub struct BitmapFrameAllocator {
    bitmap: Vec<u8>,
    frame_count: usize,
    next_hint: usize,
}

impl BitmapFrameAllocator {
    pub fn init(memory_map: &'static MemoryRegions, frames_already_allocated: usize) -> Self {
        let highest_addr = memory_map
            .iter()
            .filter(|r| matches!(r.kind, MemoryRegionKind::Usable | MemoryRegionKind::Bootloader))
            .map(|r| r.end)
            .max()
            .unwrap_or(0);
        let frame_count = (highest_addr / FRAME_SIZE) as usize;
        // Default every frame to "used"; only usable frames beyond what
        // Stage A already consumed get cleared to "free" below.
        let mut bitmap = vec![0xffu8; frame_count.div_ceil(8)];

        let mut usable_index = 0usize;
        for region in memory_map
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
        {
            let mut addr = region.start;
            while addr < region.end {
                let frame_idx = (addr / FRAME_SIZE) as usize;
                if usable_index >= frames_already_allocated {
                    Self::clear_bit(&mut bitmap, frame_idx);
                }
                usable_index += 1;
                addr += FRAME_SIZE;
            }
        }

        Self {
            bitmap,
            frame_count,
            next_hint: 0,
        }
    }

    fn is_free(&self, idx: usize) -> bool {
        (self.bitmap[idx / 8] & (1 << (idx % 8))) == 0
    }

    fn set_bit(bitmap: &mut [u8], idx: usize) {
        bitmap[idx / 8] |= 1 << (idx % 8);
    }

    fn clear_bit(bitmap: &mut [u8], idx: usize) {
        bitmap[idx / 8] &= !(1 << (idx % 8));
    }

    pub fn free_frame_count(&self) -> usize {
        (0..self.frame_count).filter(|&i| self.is_free(i)).count()
    }

    pub fn total_frame_count(&self) -> usize {
        self.frame_count
    }
}

unsafe impl FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        for offset in 0..self.frame_count {
            let idx = (self.next_hint + offset) % self.frame_count;
            if self.is_free(idx) {
                Self::set_bit(&mut self.bitmap, idx);
                self.next_hint = idx + 1;
                return Some(PhysFrame::containing_address(PhysAddr::new(
                    idx as u64 * FRAME_SIZE,
                )));
            }
        }
        None
    }
}

impl FrameDeallocator<Size4KiB> for BitmapFrameAllocator {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame) {
        let idx = (frame.start_address().as_u64() / FRAME_SIZE) as usize;
        Self::clear_bit(&mut self.bitmap, idx);
    }
}

pub static FRAME_ALLOCATOR: Mutex<Option<BitmapFrameAllocator>> = Mutex::new(None);

pub fn install_global(allocator: BitmapFrameAllocator) {
    *FRAME_ALLOCATOR.lock() = Some(allocator);
}

pub fn free_frame_count() -> usize {
    FRAME_ALLOCATOR
        .lock()
        .as_ref()
        .map(|a| a.free_frame_count())
        .unwrap_or(0)
}

pub fn total_frame_count() -> usize {
    FRAME_ALLOCATOR
        .lock()
        .as_ref()
        .map(|a| a.total_frame_count())
        .unwrap_or(0)
}

pub fn alloc_frame() -> Option<PhysFrame> {
    FRAME_ALLOCATOR.lock().as_mut().and_then(|a| a.allocate_frame())
}

/// # Safety
/// `frame` must have come from [`alloc_frame`] and not still be in use.
pub unsafe fn dealloc_frame(frame: PhysFrame) {
    if let Some(a) = FRAME_ALLOCATOR.lock().as_mut() {
        a.deallocate_frame(frame);
    }
}

/// A zero-sized `FrameAllocator` handle that delegates to the global
/// bitset allocator, for passing to `Mapper::map_to` calls made outside
/// `main.rs` (e.g. when mapping a fresh per-task stack in Phase 3).
pub struct GlobalFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for GlobalFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        alloc_frame()
    }
}
