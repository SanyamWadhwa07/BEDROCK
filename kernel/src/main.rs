#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod demo_tasks;
mod gdt;
mod graphics;
mod input;
mod interrupts;
mod memory;
mod serial;
mod shell;
mod task;
mod wm;

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use x86_64::VirtAddr;

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial_println!("Bedrock kernel booting...");

    gdt::init();
    interrupts::init_idt();
    interrupts::init_pics();
    serial_println!("GDT/TSS, IDT, and PIC remap initialized.");

    let mut usable_bytes: u64 = 0;
    let region_count = boot_info.memory_regions.len();
    for region in boot_info.memory_regions.iter() {
        if region.kind == bootloader_api::info::MemoryRegionKind::Usable {
            usable_bytes += region.end - region.start;
        }
    }
    shell::set_mem_info(usable_bytes, region_count);

    let phys_mem_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("bootloader did not map physical memory");
    let mut mapper = unsafe { memory::paging::init(VirtAddr::new(phys_mem_offset)) };
    let mut bump_allocator = memory::frame_allocator::BumpFrameAllocator::init(&boot_info.memory_regions);
    memory::heap::init_heap(&mut mapper, &mut bump_allocator).expect("heap initialization failed");
    serial_println!(
        "Heap initialized: {} bytes at {:#x}.",
        memory::heap::HEAP_SIZE,
        memory::heap::HEAP_START
    );

    let bitmap_allocator = memory::frame_allocator::BitmapFrameAllocator::init(
        &boot_info.memory_regions,
        bump_allocator.frames_allocated(),
    );
    serial_println!(
        "Frame allocator: {} free / {} total frames.",
        bitmap_allocator.free_frame_count(),
        bitmap_allocator.total_frame_count()
    );
    memory::frame_allocator::install_global(bitmap_allocator);
    memory::paging::install_global(mapper);

    if let Some(fb) = boot_info.framebuffer.as_mut() {
        let info = fb.info();
        let buffer = fb.buffer_mut();
        graphics::init(buffer, info);
        fb_println!("Bedrock kernel: Phase 6 -- window manager");
        fb_println!("GDT/TSS/IDT/PIC/paging/heap/scheduler initialized.");
    } else {
        serial_println!("no framebuffer provided by bootloader");
    }

    task::scheduler::init();
    task::scheduler::spawn(demo_tasks::task_ping);
    task::scheduler::spawn(demo_tasks::task_pong);
    task::scheduler::spawn(demo_tasks::task_c);
    task::scheduler::spawn(demo_tasks::task_d);
    task::scheduler::spawn(demo_tasks::task_e);
    task::scheduler::spawn(wm::wm_task);
    serial_println!(
        "Scheduler initialized with {} tasks (including boot task).",
        task::scheduler::task_count()
    );

    input::init_devices(); // must precede sti: consumes ACK bytes by polling
    task::timer::init_pit();
    interrupts::unmask_irqs();
    x86_64::instructions::interrupts::enable();
    serial_println!("Timer armed, IRQs unmasked (0/1/2/12), interrupts enabled.");

    serial_println!("Bedrock: entering serial shell. Try 'help'.");
    shell::run();
}

pub fn halt_loop() -> ! {
    loop {
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("PANIC: {}", info);
    halt_loop();
}
