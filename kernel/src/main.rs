#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

mod framebuffer_text;
mod gdt;
mod interrupts;
mod serial;
mod shell;

use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;

entry_point!(kernel_main);

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

    if let Some(fb) = boot_info.framebuffer.as_mut() {
        let info = fb.info();
        let buffer = fb.buffer_mut();
        framebuffer_text::init(buffer, info);
        fb_println!("Bedrock kernel: Phase 1 -- boot to shell");
        fb_println!("GDT/TSS/IDT/PIC initialized. Serial shell ready.");
    } else {
        serial_println!("no framebuffer provided by bootloader");
    }

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
