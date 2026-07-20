use crate::gdt;
use crate::serial_println;
use pic8259::ChainedPics;
use spin::{Lazy, Mutex};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }
    idt.page_fault.set_handler_fn(page_fault_handler);
    idt.general_protection_fault
        .set_handler_fn(general_protection_fault_handler);
    idt[PIC_1_OFFSET].set_handler_fn(crate::task::timer::timer_interrupt_handler);
    idt[KEYBOARD_VECTOR].set_handler_fn(keyboard_interrupt_handler);
    idt[MOUSE_VECTOR].set_handler_fn(mouse_interrupt_handler);
    idt[COM1_VECTOR].set_handler_fn(serial_interrupt_handler);
    idt
});

pub const KEYBOARD_VECTOR: u8 = PIC_1_OFFSET + 1; // IRQ1
pub const COM1_VECTOR: u8 = PIC_1_OFFSET + 4; // IRQ4
pub const MOUSE_VECTOR: u8 = PIC_2_OFFSET + 4; // IRQ12

/// Drains every byte the UART has buffered, not just one: a single IRQ4 can
/// cover several bytes that arrived close together, and leaving any behind
/// would stall the interrupt (the 16550 only re-asserts on a fresh arrival).
extern "x86-interrupt" fn serial_interrupt_handler(_stack_frame: InterruptStackFrame) {
    while crate::serial::byte_ready() {
        crate::serial::SERIAL_QUEUE.push(crate::serial::isr_read_byte());
    }
    unsafe {
        PICS.lock().notify_end_of_interrupt(COM1_VECTOR);
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let scancode = crate::input::isr_read_data_port();
    crate::input::KEYBOARD_QUEUE.push(scancode);
    unsafe {
        PICS.lock().notify_end_of_interrupt(KEYBOARD_VECTOR);
    }
}

extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let byte = crate::input::isr_read_data_port();
    // Stamp before pushing: input latency is measured from the moment the
    // hardware byte was claimed, not from whenever the WM drains the queue.
    crate::metrics::MOUSE_ISR_TSC.store(
        crate::task::timer::rdtsc(),
        core::sync::atomic::Ordering::Relaxed,
    );
    crate::input::MOUSE_QUEUE.push(byte);
    unsafe {
        PICS.lock().notify_end_of_interrupt(MOUSE_VECTOR);
    }
}

/// Explicitly unmask exactly the IRQs we handle -- timer (0), keyboard (1),
/// cascade (2, required for anything on the second PIC), COM1 serial (4),
/// mouse (12) -- and mask everything else, instead of trusting whatever mask
/// state the BIOS left behind. A 0 bit means "unmasked".
pub fn unmask_irqs() {
    unsafe {
        PICS.lock().write_masks(0b1110_1000, 0b1110_1111);
    }
}

pub fn init_idt() {
    IDT.load();
}

/// Remaps the legacy 8259 PIC so hardware IRQs land at vectors 32-47,
/// out of the way of CPU exception vectors 0-31. Interrupts are left
/// masked at the CPU level (no `sti`) until Phase 3/5 wire up real
/// handlers for the timer and keyboard/mouse IRQs.
pub fn init_pics() {
    unsafe {
        PICS.lock().initialize();
    }
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial_println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    serial_println!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
    crate::halt_loop();
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
    serial_println!("EXCEPTION: PAGE FAULT");
    serial_println!("Accessed Address: {:?}", Cr2::read());
    serial_println!("Error Code: {:?}", error_code);
    serial_println!("{:#?}", stack_frame);
    crate::halt_loop();
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println!("EXCEPTION: GENERAL PROTECTION FAULT");
    serial_println!("Error Code: {:#x}", error_code);
    serial_println!("{:#?}", stack_frame);
    crate::halt_loop();
}
