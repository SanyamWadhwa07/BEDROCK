# Bedrock -- Design

See [README.md](README.md) for the pitch, quickstart, and benchmarks. This document is the deeper "why," aimed at explaining every major tradeoff well enough to defend it in an interview. [PLAN.md](PLAN.md) has the full phased build plan plus a dated progress log of every real bug found during bring-up.

## Locked design decisions

| # | Decision | Why |
|---|---|---|
| 1 | **x86_64**, boot via the `bootloader` crate (BIOS path), no hand-rolled multiboot2/limine, no UEFI (stretch, not attempted) | Best-documented, best-tooled solo path; hand-rolling a real->protected->long-mode transition trades kernel-engineering time for boot-protocol trivia with far worse debugging support |
| 2 | Physical memory: **two-stage frame allocator** -- a bootstrap linear (bump) allocator over the bootloader's memory map, promoted to a **bitmap allocator** (1 bit/4KiB frame, rotating-cursor scan) once the heap exists. Virtual memory: the bootloader's offset-mapped physical memory + higher-half kernel via `OffsetPageTable` | A bitmap is simple and robust for fixed-size 4KiB units -- a buddy allocator's benefit (efficient variable-size allocation) doesn't apply here. Offset-mapped physical memory avoids self-referential recursive page-table bugs |
| 3 | IDT + a TSS/IST-backed double-fault stack, legacy **8259 PIC** (not APIC), single-core only | PIC is sufficient for one core; APIC only pays off for SMP, explicitly out of scope |
| 4 | **Monolithic, single-address-space, ring-0-only kernel.** All tasks (drivers, WM, apps) are kernel threads. Preemptive round-robin scheduler, context switch via a naked-fn callee-saved-register swap | Ring-3 isolation (syscalls, an ELF loader, per-process CR3) is itself a multi-month project; cutting it puts all remaining time into the GUI stack, the real differentiator versus a plain "boots to shell" kernel. The single biggest, most deliberate scope cut versus Redox's process model |
| 5 | Bootloader-provided linear framebuffer, software back-buffer, **dirty-rectangle compositing** (not naive full redraw), fixed cadence off the existing PIT timer | Dirty-rect is a real, benchmarkable perf decision, not a toy -- a naive full-screen redraw every tick would be strictly simpler to write and strictly worse |
| 6 | PS/2 keyboard (`pc-keyboard` crate) + PS/2 mouse (hand-rolled 3-byte packet parser), both **IRQ-driven** through a lock-free ring buffer | Correct OS approach, reuses the Phase-1 IDT/PIC infrastructure; USB HID is dramatically harder for no benefit inside QEMU |
| 7 | Window/surface struct with a z-order `Vec`, **immediate-mode** app drawing (no retained-mode widget toolkit) | A full widget-tree/layout engine is its own multi-month project; immediate-mode (egui-style) still proves the whole GUI stack end to end without that cost |
| 8 | **SimpleFS**, a custom on-disk filesystem (superblock + inode table + two bitmaps) over a hand-rolled **ATA PIO** driver, rather than implementing FAT32 | A custom filesystem design is more original resume/interview signal than a spec-implementation exercise; ATA PIO avoids the PCI enumeration and DMA-ring-buffer complexity AHCI would require |

## The hardest components

### 1. Physical + virtual memory manager

Stage A bump-allocates frames straight from the bootloader's memory map (no `dealloc`), used only to build the initial page tables and back a small fixed heap. Stage B, once that heap is live, allocates a bitmap (1 bit per frame across all usable RAM) *from the heap itself*, marks every Stage-A-consumed frame used, and becomes the real global frame allocator -- it scans from a last-freed cursor hint rather than restarting from zero every time, to avoid O(n) behavior under steady alloc/free churn. Paging goes through the bootloader's fixed offset-mapped physical memory plus `OffsetPageTable`; every new mapping (heap growth, per-task stacks, window buffers) goes through `map_to()` using the bitmap allocator as the `FrameAllocator`. Every per-task kernel stack gets a deliberately **unmapped guard page** directly below it, so a stack overflow page-faults immediately instead of silently corrupting whatever memory happens to sit below.

### 2. Interrupts/exceptions

A GDT plus a TSS whose `interrupt_stack_table[0]` points at a small dedicated static stack, used *only* by the double-fault handler. The IDT covers breakpoint, double-fault (`.set_stack_index(0)`), page-fault (reads `CR2`), general-protection-fault, and the remapped hardware IRQs (the 8259 PIC is remapped so IRQs land at vectors 32-47, out of the way of the CPU's own exception vectors). Why the IST stack matters: a fault inside a fault handler, combined with an already-corrupted current stack (a stack overflow being the classic case once per-task stacks exist), triggers a double fault; without a guaranteed-good stack reserved for that handler specifically, the CPU can't deliver the exception at all -- the result is a **triple fault**, which just silently reboots the machine with zero diagnostic information, the single most confusing bare-metal failure mode there is. The IST switch happens at the hardware level before any handler code runs, completely decoupled from whatever stack was already faulting -- this is what turns "mystery instant reboot" into a clean diagnostic printout.

### 3. Preemptive scheduler

Each task is a `Tcb { id, saved_rsp, state }`. Because the kernel is single-address-space, no `CR3` reload is needed on a switch. The ready queue is a round-robin `VecDeque<TaskId>` with a fixed time slice measured in PIT ticks. The context switch itself is a `#[unsafe(naked)] fn switch_to(prev, next)`: it pushes the System-V-ABI callee-saved registers (`rbx, rbp, r12-r15`) onto the outgoing task's own stack, swaps `rsp` to the incoming task's saved value, pops that task's own previously-saved registers, and `ret`s. The timer IRQ handler itself stays thin -- bump the tick counter, call `schedule()` only if the time slice expired, otherwise just acknowledge and return.

The subtle part: because every non-running task's stack is arranged to look "mid-call inside `switch_to`, about to `ret`," the final `ret` either unwinds back into that task's own previously-interrupted call frame (which resumes via its own saved `iretq`), or -- for a brand-new task -- lands in a small trampoline that explicitly re-enables interrupts before jumping to the real entry point. That trampoline exists because of a real bug hit during bring-up: entering the timer ISR clears the CPU's interrupt-enable flag (standard interrupt-gate semantics), normally restored by the `iretq` a *resumed* task's own history goes through -- but a brand-new task has no such history, so without the trampoline's explicit `sti`, the interrupt flag stayed cleared forever after the very first switch into any fresh task, and the timer simply never fired again. This is exactly the reason preemptive switching doesn't compose cleanly with Rust's `x86-interrupt` ABI on its own, and why some blog-post-style toy kernels stop at cooperative (`async`) scheduling instead of going this far.

### 4. Framebuffer + compositor

A heap-allocated `Vec<u8>` back buffer matches the bootloader-reported `PixelFormat` exactly (`Rgb` and `Bgr` are both handled explicitly, since hardcoding one swaps colors on some configurations). All drawing targets the back buffer; dirty-rect tracking accumulates a small fixed-capacity list of rects per frame (window moves/resizes/creates/destroys, cursor movement, per-window content animation), greedily merging any two that overlap or touch via bounding-box union, and collapsing everything into one full-screen rect if the list still fragments past a cap -- the "give up and repaint one big region" fallback real compositors use. Each redraw tick, for every rect that's actually dirty: repaint the background, then composite every window whose bounds touch that rect back-to-front in z-order (painter's algorithm -- later windows simply overwrite earlier ones in the overlap, which handles occlusion correctly without needing per-pixel clipping), then copy just that rect into the real MMIO framebuffer.

### 5. Window manager

A `Window` owns its rect, title, and an immediate-mode `WindowContent` (no retained widget tree -- each app variant draws itself directly against the back buffer every frame it's dirty). Z-order is a back-to-front `Vec<usize>` of indices into the window table; hit-testing walks it top-down (reverse) so the frontmost window wins overlapping clicks, and raising a window is a remove-then-push-to-the-end. Closing a window requires reindexing every z-order entry pointing past the removed slot, plus falling back the focus pointer to the new topmost survivor if the closed window happened to be focused -- both of these were the two trickiest bookkeeping cases and were specifically covered by a temporary self-test before being trusted (see PLAN.md's Phase 6 log). Mouse-down hit-testing checks, in order, the close button, the resize-handle corner, the titlebar (start a move-drag), then the window body (just focus/raise) -- all computed from the window's own current rect, not fixed screen coordinates, so they stay correct after every drag/resize.

### 6. On-disk filesystem (SimpleFS) + ATA PIO driver

ATA PIO over the legacy fixed ports (0x1F0-0x1F7, plus 0x3F6 for the alternate-status/device-control register), targeting the primary channel's **slave** device specifically so the boot disk on the primary master is never touched -- no PCI enumeration, one `in`/`out` per register, polled status. On-disk layout is `[superblock][inode bitmap][data bitmap][inode table][data blocks]`, 4KiB blocks (8 ATA sectors each), xv6/early-ext2-style. Each inode holds 10 direct block pointers plus one single-indirect block (a shallow, B-tree-like fanout: `(10 + 1024) * 4KiB` max file size from just one extra indirection level). The root directory is a single fixed data block of `(name, inode)` entries -- flat, no subdirectories, a deliberate scope cut so Phase 7 could focus on proving read+write+persistence end-to-end rather than directory-tree traversal.

**A real bug worth calling out specifically**: this driver is polling-only by design (that's the entire point of PIO), and never touches interrupts on purpose -- but every real ATA controller still raises IRQ14 on its own after each command completes, *regardless* of whether the driver asked for interrupt-driven operation. IRQ14 was masked at the PIC (only the timer, keyboard, PIC cascade, and mouse IRQs are ever unmasked), which in theory should have made this harmless -- but empirically, the very first time interrupts were ever globally enabled after any ATA operation had run, the kernel double-faulted, reproducibly. The fix was to explicitly set the "no interrupt enable" (nIEN) bit in the ATA device control register before any transfer, silencing the controller's own IRQ14 generation entirely. Bisection to find this took longer than the fix itself -- see PLAN.md's Phase 7 log for the full diagnostic trail (it was found by systematically ruling out the just-changed task-stack size and the SimpleFS call depth first, before narrowing it down to *any* single raw `ata::read_sector` call, and finally to the specific missing register write).

## DSA & algorithms map

Every core component is built on a *named*, classic data structure or algorithm -- cite these by name when walking through the project:

| Component | Named DSA / algorithm | Where |
|---|---|---|
| Physical frame allocator | **Bitset / bitmap** (bit manipulation: `idx/8`, `1 << (idx%8)`), rotating-cursor scan to amortize O(n) | `kernel/src/memory/frame_allocator.rs` |
| Kernel heap | **Free-list allocation** (`linked_list_allocator`) over pages mapped by the frame allocator | `kernel/src/memory/heap.rs` |
| Scheduler | **Round-robin circular queue** (`VecDeque` rotation of task indices), O(1) enqueue/dequeue | `kernel/src/task/scheduler.rs` |
| Context switch | **Callee-saved register contract** (System V AMD64 ABI) + fabricated stack frames | `kernel/src/task/context_switch.rs` |
| Dirty-rect compositing | **Interval/rectangle union** (bounding-box merge with a fragmentation cap + full-redraw fallback) + **painter's algorithm** (back-to-front z-order composite) | `kernel/src/graphics/dirty_rect.rs`, `kernel/src/wm/wm.rs` |
| Input event queues | **Lock-free SPSC ring buffer** (circular buffer, atomic head/tail -- single-producer ISR, single-consumer task) | `kernel/src/input/event_queue.rs` |
| PS/2 mouse protocol | **Finite state machine** with sync-bit recovery (3-byte packet reassembly, two's-complement deltas) | `kernel/src/input/mouse.rs` |
| Window manager | **Z-order list** + top-down **hit-testing** (point-in-rect search) | `kernel/src/wm/zorder.rs` |
| SimpleFS | **Inode tree indexing** (direct + single-indirect blocks = shallow B-tree-like fanout), **two bitmaps** for free-space tracking, directory as an association list | `kernel/src/fs/simplefs.rs` |

**Not implemented this session** (named as explicit future work, not silently skipped): an LRU block cache for the filesystem (hashmap + doubly-linked list, O(1) get/evict -- would directly address the ATA throughput numbers in the README), and an MLFQ (multilevel feedback queue) scheduler upgrade.

## Interview-defense checklist

1. `bootloader` crate vs. hand-rolled multiboot2/limine -- maturity/velocity vs. knowing exactly what a hand-rolled real->protected->long-mode transition would cost.
2. Bitmap physical allocator vs. buddy -- bitmap + a cursor hint is simpler, and buddy's actual benefit (efficient variable-size allocation) doesn't apply to fixed 4KiB frames.
3. PIC vs. APIC / no SMP -- PIC is right-sized for single-core; APIC only matters once there's more than one core to route interrupts to.
4. Monolithic ring-0-only kernel vs. a Redox-style microkernel with ring-3 isolation -- the single biggest named scope cut in the whole project; know exactly what syscalls, per-process `CR3`, and ELF loading would cost, traded deliberately for a complete GUI stack instead.
5. Dirty-rect compositing vs. naive full redraw -- real bookkeeping complexity taken on specifically for a benchmarkable perf win, not because it was the easy path.
6. Custom on-disk filesystem vs. FAT32 -- original design/recovery-story signal vs. a spec-implementation exercise.
7. ATA PIO vs. AHCI/DMA -- a deliberate, honestly-acknowledged scope cut, directly visible in the throughput benchmark in the README.
8. The nIEN/IRQ14 bug (see "the hardest components" above) -- a good example of a bug that pure code review would never have caught, because the driver code itself was "correct" for what it intended to do; it only surfaced by actually booting and enabling interrupts for real.
