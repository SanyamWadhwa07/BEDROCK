use spin::Lazy;
use x86_64::instructions::segmentation::{Segment, CS, DS, ES, FS, GS, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::PrivilegeLevel;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

const STACK_SIZE: usize = 4096 * 5;

static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
        static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
        let stack_start = VirtAddr::from_ptr(core::ptr::addr_of!(STACK));
        stack_start + STACK_SIZE as u64
    };
    tss
});

struct Selectors {
    code_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code_selector = gdt.append(Descriptor::kernel_code_segment());
    let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));
    (gdt, Selectors { code_selector, tss_selector })
});

pub fn init() {
    GDT.0.load();
    // The bootloader's own (now-discarded) GDT left stale selector values in
    // SS/DS/ES/FS/GS. Our new GDT is much smaller, so those stale indices may
    // no longer point at a valid/compatible descriptor (or may alias our TSS
    // descriptor). Null them out explicitly -- in 64-bit mode a null selector
    // is valid to load into every register except CS.
    let null_selector = SegmentSelector::new(0, PrivilegeLevel::Ring0);
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        SS::set_reg(null_selector);
        DS::set_reg(null_selector);
        ES::set_reg(null_selector);
        FS::set_reg(null_selector);
        GS::set_reg(null_selector);
        load_tss(GDT.1.tss_selector);
    }
}
