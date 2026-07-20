# Captured benchmark sessions

Raw serial logs behind the numbers in the top-level [README](../../README.md), kept so the figures can be checked against the actual capture rather than taken on trust.

- `phase9-session.log`: one continuous boot covering the context-switch, allocator, filesystem, and raw ATA benchmarks, the block cache measured with `fscache` both on and off, and the compositor measured with `redraw` in both naive and dirty-rect modes. Also shows the boot sequence and the scheduler fairness snapshot from `tasks`.
- `phase9-persistence.log`: three independent QEMU processes against one `fs.img`. The first writes a file, the second reads it back byte-for-byte and renames it, the third sees the rename and deletes it. Captured after the filesystem layout was extracted into `libs/bedrock-fs`, to confirm the on-disk format is unchanged.

## Reproducing

Boot the kernel and type the commands into the serial console:

```
cargo xtask run --release
```

Then, at the `bedrock>` prompt: `bench all`, `metrics`, `fscache off`, `bench fs`, `redraw naive`, `metrics`, and so on. Everything in the tables comes from commands built into the shipped kernel.

Two notes if you script the console rather than typing by hand. Feed characters with a small gap between them: QEMU hands the guest an entire pipe write at once, which outruns the emulated UART's receive path regardless of how fast the guest drains it. And run QEMU with a display if you want the input-latency figure, since a headless run generates no mouse packets and `metrics` will honestly report 0.

## Sandbox

QEMU 11.0 on Windows with no hardware acceleration available, so pure TCG software emulation. Absolute numbers are specific to that setup and meaningfully slower than KVM/WHPX or real hardware, particularly for PIO transfers and the compositor's MMIO writes. The relative comparisons are the part worth reading.
