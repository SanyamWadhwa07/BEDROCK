use spin::Mutex;
use x86_64::structures::paging::{OffsetPageTable, PageTable};
use x86_64::VirtAddr;

pub static MAPPER: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);

pub fn install_global(mapper: OffsetPageTable<'static>) {
    *MAPPER.lock() = Some(mapper);
}

pub fn with_mapper<R>(f: impl FnOnce(&mut OffsetPageTable<'static>) -> R) -> R {
    let mut guard = MAPPER.lock();
    f(guard.as_mut().expect("page table mapper not initialized"))
}

/// Builds an `OffsetPageTable` over the bootloader's currently-active level-4
/// page table, using the fixed offset at which the bootloader has already
/// mapped all physical memory into our address space.
///
/// # Safety
/// `physical_memory_offset` must be the correct offset, and this must only
/// be called once (it hands out a `&'static mut` to the live page table).
pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = active_level_4_table(physical_memory_offset);
    OffsetPageTable::new(level_4_table, physical_memory_offset)
}

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    use x86_64::registers::control::Cr3;

    let (level_4_table_frame, _) = Cr3::read();
    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();
    &mut *page_table_ptr
}
