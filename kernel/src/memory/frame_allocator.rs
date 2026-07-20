use bedrock_mm::FrameBitmap;
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

/// Stage B: a bitset allocator covering physical memory from address 0 up to
/// the end of the highest usable/bootloader region. Built from the heap (so
/// it needs the heap to already exist), promoting from `BumpFrameAllocator`
/// by marking every frame Stage A already handed out as used.
///
/// The bit manipulation itself (1 bit per 4KiB frame packed into `u8` words,
/// the rotating scan cursor, the free-count bookkeeping) lives in the
/// `bedrock-mm` crate, where it is host-testable under ordinary `cargo test`
/// -- including a randomized test asserting the property this kernel's
/// stability rests on: a frame is never handed out twice before being freed.
/// What stays here is only the part that genuinely needs the target: the
/// bootloader memory-map walk and the frame-index <-> `PhysAddr` arithmetic.
pub struct BitmapFrameAllocator {
    bitmap: FrameBitmap,
}

impl BitmapFrameAllocator {
    pub fn init(memory_map: &'static MemoryRegions, frames_already_allocated: usize) -> Self {
        let highest_addr = memory_map
            .iter()
            .filter(|r| {
                matches!(
                    r.kind,
                    MemoryRegionKind::Usable | MemoryRegionKind::Bootloader
                )
            })
            .map(|r| r.end)
            .max()
            .unwrap_or(0);
        let frame_count = (highest_addr / FRAME_SIZE) as usize;
        // Every frame starts marked "used"; only usable frames beyond what
        // Stage A already consumed get freed below. Defaulting to "used"
        // means a frame missing from the memory map is never handed out.
        let mut bitmap = FrameBitmap::new_all_allocated(frame_count);

        let mut usable_index = 0usize;
        for region in memory_map
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
        {
            let mut addr = region.start;
            while addr < region.end {
                if usable_index >= frames_already_allocated {
                    bitmap.set_free((addr / FRAME_SIZE) as usize);
                }
                usable_index += 1;
                addr += FRAME_SIZE;
            }
        }

        Self { bitmap }
    }

    pub fn free_frame_count(&self) -> usize {
        self.bitmap.free_count()
    }

    pub fn total_frame_count(&self) -> usize {
        self.bitmap.frame_count()
    }
}

unsafe impl FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let idx = self.bitmap.alloc()?;
        Some(PhysFrame::containing_address(PhysAddr::new(
            idx as u64 * FRAME_SIZE,
        )))
    }
}

impl FrameDeallocator<Size4KiB> for BitmapFrameAllocator {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame) {
        self.bitmap
            .free((frame.start_address().as_u64() / FRAME_SIZE) as usize);
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
    FRAME_ALLOCATOR
        .lock()
        .as_mut()
        .and_then(|a| a.allocate_frame())
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
