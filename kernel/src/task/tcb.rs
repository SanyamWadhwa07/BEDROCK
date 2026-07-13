use crate::memory::{frame_allocator, paging};
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::structures::paging::{Mapper, Page, PageTableFlags, Size4KiB};
use x86_64::VirtAddr;

pub type TaskId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
}

pub struct Tcb {
    pub id: TaskId,
    pub saved_rsp: u64,
    pub state: TaskState,
}

const STACK_PAGES: u64 = 8; // 32 KiB per task -- plenty for these demo tasks
const GUARD_PAGES: u64 = 1;
const SLOT_SIZE: u64 = (STACK_PAGES + GUARD_PAGES) * 4096;
const STACK_AREA_BASE: u64 = 0x_5555_5555_0000;

static NEXT_STACK_SLOT: AtomicU64 = AtomicU64::new(0);

/// Allocates a fresh, mapped kernel stack for a new task, with a
/// deliberately UNMAPPED guard page directly below it: a stack overflow
/// touches that page and page-faults immediately (feeding into the
/// IST-backed double-fault path already proven in Phase 1) instead of
/// silently corrupting whatever memory happens to sit below.
pub fn alloc_task_stack() -> VirtAddr {
    let slot = NEXT_STACK_SLOT.fetch_add(1, Ordering::Relaxed);
    let slot_base = STACK_AREA_BASE + slot * SLOT_SIZE;
    // The slot's first page is the guard page -- deliberately left unmapped.
    let stack_start = VirtAddr::new(slot_base + 4096);
    let stack_end = stack_start + STACK_PAGES * 4096 - 1u64;

    paging::with_mapper(|mapper| {
        let start_page = Page::<Size4KiB>::containing_address(stack_start);
        let end_page = Page::<Size4KiB>::containing_address(stack_end);
        for page in Page::range_inclusive(start_page, end_page) {
            let frame = frame_allocator::alloc_frame().expect("out of physical frames for task stack");
            let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
            unsafe {
                mapper
                    .map_to(page, frame, flags, &mut frame_allocator::GlobalFrameAllocator)
                    .expect("failed to map task stack page")
                    .flush();
            }
        }
    });

    stack_start + STACK_PAGES * 4096 // top of stack; stacks grow down from here
}

/// Lays out a brand-new task's initial stack so that `context_switch::switch_to`'s
/// restore sequence (`pop r15; pop r14; pop r13; pop r12; pop rbp; pop rbx; ret`)
/// pops five zeroed callee-saved registers plus `entry` into r12, then `ret`s
/// into `task_trampoline` (which `sti`s -- see its doc comment for why that's
/// required -- then jumps to whatever's in r12).
pub fn prepare_initial_stack(stack_top: VirtAddr, entry: extern "C" fn() -> !) -> u64 {
    unsafe {
        let mut sp = stack_top.as_u64();
        sp -= 8;
        *(sp as *mut u64) = super::context_switch::task_trampoline_addr(); // `ret` target
        sp -= 8;
        *(sp as *mut u64) = 0; // rbx
        sp -= 8;
        *(sp as *mut u64) = 0; // rbp
        sp -= 8;
        *(sp as *mut u64) = entry as *const () as u64; // r12 -- read by task_trampoline
        sp -= 8;
        *(sp as *mut u64) = 0; // r13
        sp -= 8;
        *(sp as *mut u64) = 0; // r14
        sp -= 8;
        *(sp as *mut u64) = 0; // r15
        sp
    }
}
