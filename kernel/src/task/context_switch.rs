/// Saves the outgoing task's callee-saved registers (System V AMD64 ABI:
/// rbx, rbp, r12-r15) onto its own stack, stashes the resulting `rsp` into
/// `*prev_rsp_out`, then switches `rsp` to `next_rsp` and pops the incoming
/// task's own previously-saved registers before `ret`-ing.
///
/// Because every non-running task's stack is arranged to look "mid-call
/// inside `switch_to`, about to `ret`" -- either because it was genuinely
/// switched away from here before, or because [`crate::task::tcb::prepare_initial_stack`]
/// faked that shape for a brand-new task -- the final `ret` lands either back
/// in that task's own previously-interrupted call site (which itself resumes
/// via its own saved interrupt frame's `iretq`) or straight into a new task's
/// entry point. This is the standard resolution used by xv6/Redox-lineage
/// kernels for the fact that preemptive switching doesn't compose cleanly
/// with Rust's `x86-interrupt` ABI on its own.
///
/// # Safety
/// `next_rsp` must point at a stack previously prepared either by this same
/// function mid-switch, or by [`crate::task::tcb::prepare_initial_stack`].
/// `prev_rsp_out` must point at storage that outlives this task's next
/// switch back in.
#[unsafe(naked)]
pub unsafe extern "C" fn switch_to(prev_rsp_out: *mut u64, next_rsp: u64) {
    core::arch::naked_asm!(
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov [rdi], rsp", // prev_rsp_out = rsp (first arg, rdi)
        "mov rsp, rsi",   // rsp = next_rsp (second arg, rsi)
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",
        "ret",
    );
}

/// A resumed task's `ret` above lands back in its own previously-interrupted
/// call site, which eventually `iretq`s and restores its own saved RFLAGS
/// (including the interrupt-enable flag, IF) from the frame the CPU pushed
/// when it was originally interrupted. A *brand-new* task has no such
/// history -- [`crate::task::tcb::prepare_initial_stack`] fakes its initial
/// stack so `switch_to`'s `ret` lands here instead of directly in the task's
/// real entry point, specifically so we can `sti` first. Skipping this step
/// is a real bug we hit during Phase-3 bring-up: entering the timer ISR
/// clears IF (interrupt gate semantics), and since a new task's first run
/// never goes through the matching `iretq`, IF stayed cleared forever after
/// the very first switch to a fresh task -- the timer simply never fired
/// again. The real entry point's address travels in r12 (one of the
/// callee-saved registers `switch_to` already restores), since a naked
/// function can't receive a normal Rust argument.
#[unsafe(naked)]
extern "C" fn task_trampoline() -> ! {
    core::arch::naked_asm!(
        "sti",
        "call r12",
        // `entry` is `-> !` and must never return, but if it somehow did:
        "2:",
        "hlt",
        "jmp 2b",
    );
}

pub(super) fn task_trampoline_addr() -> u64 {
    task_trampoline as *const () as u64
}
