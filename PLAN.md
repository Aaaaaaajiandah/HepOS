# HepOS — Complete Design Reference

> **Purpose:** This document is the authoritative reference for HepOS development.
> It survives context compaction and contains everything needed to continue building.
> **Last updated:** 2026-06-29

---

## Overview

HepOS is a custom x86\_64 operating system written in Rust. It uses an **exokernel architecture** — the kernel does as little as possible (secure hardware multiplexing), pushing all OS abstractions into userspace libOS libraries. Single user, no permissions, no networking yet.

**Language:** Rust (nightly, `no_std` + `alloc`)
**Target:** x86\_64, bare metal
**Bootloader:** Limine v9.x (BIOS mode)
**Development machine:** Windows 11 with QEMU 11.x
**License:** MIT
**Repository:** https://github.com/The-Hep-Group/HepOS

---

## Repository Structure

```
HepOS/
├── build.ps1              # One-shot build + QEMU launch script
├── PLAN.md                # This file
├── LICENSE                # MIT license
├── .gitignore             # Excludes: limine/, iso_root/, hepos.iso, hepos_disk.img, kernel/target/
├── bootloader/
│   └── limine.conf        # Boot menu config
├── kernel/
│   ├── Cargo.toml         # no_std, limine 0.6, spin 0.9, alloc
│   ├── linker.ld          # Higher-half kernel linker script (0xFFFFFFFF80000000)
│   ├── .cargo/config.toml # Target: x86_64-unknown-none, build-std, linker flags
│   └── src/
│       ├── main.rs        # kmain entry, global state, task setup, app rendering
│       ├── framebuffer.rs # GOP framebuffer pixel/rect/text renderer (8×8 bitmap font)
│       ├── gdt.rs         # Global Descriptor Table (null, code64, data64)
│       ├── idt.rs         # IDT + exception stubs + timer stub
│       ├── pmm.rs         # Physical Memory Manager (bitmap, pages above 1MB only)
│       ├── vmm.rs         # HHDM offset storage, phys_to_virt()
│       ├── paging.rs      # Page table walker, map_page(), map_mmio()
│       ├── heap.rs        # Bump allocator (1MB, no-free) — GlobalAlloc impl
│       ├── apic.rs        # x2APIC (MSR-based), timer at ~10ms, disables legacy PIC
│       ├── acpi.rs        # ACPI shutdown (port 0x604) + PS/2 reboot
│       ├── rtc.rs         # CMOS real-time clock (hour, min, sec, date)
│       ├── scheduler.rs   # Round-robin preemptive scheduler, context_switch (naked asm)
│       ├── pci.rs         # PCI enumeration via config space I/O ports (0xCF8/0xCFC)
│       ├── ps2.rs         # PS/2 keyboard (scancode set 1 + extended 0xE0 prefix → arrow keys)
│       ├── mouse.rs       # PS/2 mouse AUX port (3-byte packets, relative movement)
│       ├── nvme.rs        # NVMe driver (admin + I/O queues, global CONTROLLER static)
│       ├── hepfs.rs       # Custom filesystem (flat inodes, 4KB blocks, NVMe backend)
│       ├── desktop.rs     # Desktop environment (compositor, WM, taskbar, RTC clock)
│       ├── terminal.rs    # Full shell terminal (history, cd, ls, cat, mkdir, rm, write...)
│       ├── serial.rs      # COM1 serial debug output (print, print_hex)
│       └── panic.rs       # Panic handler (spins forever)
└── limine/                # Cloned at build time (gitignored)
```

---

## Build System

**`build.ps1`** does everything:
1. `cargo +nightly build --release` in `kernel/`
2. Clones Limine v9.x-binary if not present
3. Assembles ISO image using `xorriso` (from MSYS2 `usr/bin/`)
4. Installs Limine BIOS stage with `limine.exe`
5. Creates `hepos_disk.img` (512MB raw NVMe disk) if not present
6. Launches QEMU

**QEMU command:**
```
qemu-system-x86_64
  -M q35
  -cpu qemu64,+x2apic     ← x2APIC required
  -m 256M
  -cdrom hepos.iso
  -boot d
  -drive file=hepos_disk.img,if=none,id=nvme0,format=raw
  -device nvme,serial=heposv1,drive=nvme0
  -vga std
  -display sdl,window-close=off
  -serial stdio
  -no-reboot
  -no-shutdown
```

**Requirements:**
- Rust nightly: `x86_64-unknown-none` target, `rust-src`, `llvm-tools`
- MSYS2 with `xorriso` in `/usr/bin/`
- QEMU 11.x at `C:\Program Files\qemu\`
- PATH must include `%USERPROFILE%\.cargo\bin`

---

## Architecture Decisions

### Exokernel Design
Kernel does only: hardware multiplexing, secure isolation, interrupt routing. Everything else (FS, scheduler policy, device abstraction) in "libOS" — currently in kernel address space. True userspace needs syscall interface + per-process paging (future).

### Memory Layout
```
0xFFFF800000000000 – 0xFFFF8FFFFFFFFFFF : HHDM (Limine maps all physical RAM)
0xFFFFFFFF80000000 – 0xFFFFFFFFFFFFFFFF : Kernel (linked here)
```

### PMM
- Bitmap, 1 bit per 4KB page, atomic u64 array
- **Only frees pages above 1MB** — avoids 0xA0000–0xFFFFF reserved hole
- alloc_page() returns physical addresses

### Heap
- **Bump allocator** — sequential, dealloc is no-op, 256 pages = 1MB
- Assumes PMM pages above 1MB are contiguous (true on QEMU)
- `#[global_allocator]` — gives `Box`, `Vec`, `String` etc.
- Replace with slab when memory pressure matters

### Paging
- map_page(virt, phys, flags) — walks PML4→PDPT→PD→PT, creates new tables from PMM
- map_mmio(phys, size) — maps MMIO at `hhdm_offset + phys` with NOCACHE flag
- Used for NVMe BAR (0xFEBD4000 on QEMU)

### APIC & Timer
- x2APIC (MSR 0x800+), no MMIO needed
- Periodic, divide-by-16, ~625K count → 10ms tick
- Vector 0x20, timer_stub saves/restores all scratch regs

### Scheduler
- Round-robin preemptive, single core, 2 tasks
- context_switch(old_rsp, new_rsp) — naked fn, saves/restores RBX/RBP/R12–R15
- CLI during NVMe init to prevent timer mid-MMIO

### Focus System
- `FOCUSED_WIN: Mutex<Option<usize>>` — None = cursor mode, Some(id) = window focused
- **ESC** → cursor mode (yellow crosshair, WASD moves)
- **Space** over window → focus it (white crosshair, all keys to that app)
- Default: Terminal window focused on boot

---

## Desktop Environment

### Windows (initial layout)
| Window | Position | Size | Content |
|--------|----------|------|---------|
| Welcome to HepOS | (20, 50) | 300×160 | System info, RAM, NVMe status |
| HepFS | (340, 50) | 260×160 | Live directory listing of cwd |
| Terminal | (20, 240) | 580×200 | Full shell with history |

### Taskbar
- Fixed 32px at bottom
- App buttons (click to focus/toggle minimize)
- **Live RTC clock** (top-right, updated every render)

### Rendering pipeline (task_blink, ~60fps)
1. Desktop render (background, windows, cursor)
2. Terminal overlay in Terminal window
3. HepFS listing overlay in HepFS window
4. Welcome info overlay in Welcome window
5. Cursor/mode indicator

---

## Terminal Shell

### Commands
| Command | Description |
|---------|-------------|
| `help` | List all commands |
| `pwd` | Print working directory |
| `ls [path]` | List directory (files show size, dirs show /) |
| `cd <dir>` | Change directory (supports `..` and `/`) |
| `cat <file>` | Print file contents |
| `mkdir <name>` | Create directory |
| `touch <name>` | Create empty file |
| `rm <name>` | Remove file or empty directory |
| `write <file> <text>` | Write text to file (creates if absent) |
| `echo <text>` | Print text |
| `history` | Show command history |
| `date` | Show current date and time (RTC) |
| `sysinfo` | Full kernel info panel |
| `uname` | System name + version |
| `mem` | RAM usage |
| `clear` | Clear screen |
| `shutdown` | ACPI power off |
| `reboot` | PS/2 controller reboot |

### Shell Features
- **Working directory** tracking (`cwd_ino` + `cwd_path`)
- **Command history** (50 entries, ↑/↓ arrows to navigate)
- **Extended PS/2 scancodes** — arrow keys, Del, Home, End
- Prompt shows current path: `/ $ ` or `/home $ `
- Ctrl+L — clear screen

---

## HepFS Filesystem

**Disk layout (4KB blocks):**
```
Block 0       : Superblock (magic, sizes, counts)
Block 1       : Inode bitmap  (32,768 bits)
Blocks 2–5    : Block bitmap  (131,072 bits = covers 512MB disk)
Blocks 6–37   : Inode table   (1,024 inodes, 128 bytes each)
Blocks 38+    : Data blocks
```

**Inode (128 bytes):** flags, size, nblocks, ctime, mtime, direct[12] block pointers  
**Directory entry (32 bytes):** inode, name_len, name[27]  
**Max file size:** 12 × 4096 = 49,152 bytes (direct blocks only)

**Public API:**
- `format(ctrl)`, `probe(ctrl)` — format/check
- `lookup(ctrl, "/path")` → `Option<u32>` — path resolution
- `create_file/create_dir(ctrl, parent, name)` → inode id
- `write_file(ctrl, ino, data)`, `read_file(ctrl, ino)` → `Vec<u8>`
- `list_dir(ctrl, ino)` → `Vec<(u32, String)>`
- `remove(ctrl, parent, name)` → bool — deletes file or empty dir
- `stat(ctrl, ino)` → `(is_dir, size)`

**Kernel manifest:** `/kernel.txt` written at boot with version info, subsystem list, date, repo URL.

---

## NVMe Driver

- Finds device via PCI (class 0x01, subclass 0x08)
- Maps BAR0 via `paging::map_mmio()`
- Admin queues + I/O queues (depth 64 each)
- Global `CONTROLLER: Mutex<Option<NvmeController>>` — accessible from all modules
- `read_blocks(lba, count, phys_buf)`, `write_blocks(...)` — polling, no IRQ
- LBA size hardcoded 512 (QEMU default)

---

## What's Built (✓) vs What's Left (○)

### Kernel
| Component | Status | Notes |
|-----------|--------|-------|
| Boot (Limine) | ✓ | |
| Framebuffer + text | ✓ | 8×8 bitmap font, scaling |
| GDT | ✓ | |
| IDT + exceptions | ✓ | Red screen on fault |
| PMM | ✓ | Bitmap, >1MB |
| HHDM/VMM | ✓ | |
| Paging (map_mmio) | ✓ | |
| Heap | ✓ | Bump 1MB |
| x2APIC + timer | ✓ | 10ms |
| ACPI shutdown/reboot | ✓ | |
| RTC clock | ✓ | CMOS |
| Preemptive scheduler | ✓ | Round-robin |
| Focus system | ✓ | ESC/space |
| Syscall interface | ○ | SYSCALL/SYSRET for userspace |
| Per-process paging | ○ | |
| TSS | ○ | |
| Process manager | ○ | |

### Drivers
| Driver | Status | Notes |
|--------|--------|-------|
| PCI enumeration | ✓ | |
| PS/2 keyboard | ✓ | + extended scancodes (arrow keys) |
| PS/2 mouse | ✓ (driver) | QEMU doesn't route to PS/2 AUX in SDL; workaround: focus+WASD |
| NVMe | ✓ | Global controller |
| XHCI (USB 3.0) | ○ | Needed for real mouse on hardware |
| USB HID | ○ | After XHCI |
| Intel HDA (audio) | ○ | |
| RTL8169 (Ethernet) | ○ | User's NIC |
| Intel e1000e | ○ | |
| Intel i225/i226 | ○ | |
| virtio-net | ○ | QEMU |
| ACPI (full FADT) | ○ | Currently QEMU-hardcoded port |

### Storage / Filesystem
| Component | Status | Notes |
|-----------|--------|-------|
| HepFS format/probe | ✓ | |
| File CRUD | ✓ | create, read, write, delete |
| Directory CRUD | ✓ | create, list, lookup, delete |
| Path resolution | ✓ | |
| Kernel manifest | ✓ | /kernel.txt written at boot |
| VFS layer | ○ | |
| Large files (>49KB) | ○ | Indirect block support |

### Desktop Environment
| Component | Status | Notes |
|-----------|--------|-------|
| Compositor (dirty rects) | ✓ | |
| Floating window manager | ✓ | Drag, z-order, focus |
| Taskbar | ✓ | App buttons + live clock |
| Focus system | ✓ | ESC=cursor, space=focus |
| Window content rendering | ✓ | Terminal, HepFS, Welcome windows |
| Physical mouse | ○ | Needs XHCI |
| Animations | ○ | |
| Desktop icons | ○ | |

### Apps / Shell
| Component | Status | Notes |
|-----------|--------|-------|
| Terminal + shell | ✓ | Full command set |
| Command history | ✓ | 50 entries, arrow navigation |
| HepFS file manager | ✓ (basic) | List in window, navigate via terminal |
| Text editor | ○ | |
| Image viewer | ○ | |
| Audio/video player | ○ | |
| Settings | ○ | |

### Networking
| Component | Status |
|-----------|--------|
| NIC driver (any) | ○ |
| smoltcp | ○ |

### File Formats
| Format | Status |
|--------|--------|
| Plain text | ✓ (cat command) |
| PNG/JPG/MP3 etc. | ○ |

---

## Recommended Next Steps

### Immediate
1. **XHCI USB controller** — enables real mouse on hardware and USB keyboard
2. **USB HID** — after XHCI
3. **Text editor** — edit files in-OS (builds on terminal + HepFS)

### Short term
4. **Slab/pool allocator** — replace bump heap for proper free()
5. **ACPI full (FADT)** — proper power management on real hardware
6. **RTL8169 NIC driver** — user's actual hardware NIC
7. **smoltcp** — TCP/IP stack

### Medium term
8. **std shim** — allow `std`-using Rust crates to link
9. **Symphonia** — audio decoding
10. **Image crate** — PNG/JPG display

### Long term
11. **Syscall interface** (ring 3 userspace)
12. **Per-process virtual memory**
13. **File permissions** (if desired)

---

## Known Issues

| Issue | Cause | Fix |
|-------|-------|-----|
| Physical mouse no move | QEMU SDL routes to absolute ptr, not PS/2 AUX | XHCI + USB HID |
| Heap can't free | Bump allocator | Replace with slab |
| NVMe disk size 0 MB | Identify Namespace hangs on QEMU | Hardcoded 512B LBA |
| Files max 49KB | Only 12 direct inode blocks | Add indirect block |
| ACPI only works on QEMU | Hardcoded port 0x604 | Parse FADT for real HW |
| No networking | Not implemented | RTL8169 + smoltcp |

---

## Hardware / QEMU Details

| Item | Value |
|------|-------|
| RAM | 256MB |
| NVMe disk | 512MB (`hepos_disk.img`) |
| NVMe BAR | 0xFEBD4000 |
| HHDM offset | 0xFFFF800000000000 |
| APIC timer | ~10ms, vector 0x20 |
| COM1 | 0x3F8, 38400 baud |
| CMOS clock | ports 0x70/0x71 |
| ACPI shutdown | port 0x604 → 0x2000 |

---

## Development Tips

- **Serial output** → PowerShell terminal (launched by build.ps1)
- **Exception crashes** → red screen with vector/error/RIP
- **ESC** → cursor mode (yellow crosshair, WASD moves)
- **Space on window** → focus that window
- **Ctrl+L** → clear terminal
- **↑/↓ arrows** → shell history
- **`sysinfo`** → full kernel info from inside the OS
- **`cat /kernel.txt`** → kernel manifest written at boot
- **Adding a module:** mod in main.rs, call init() in boot sequence
- **Adding a terminal command:** add arm to `execute()` in terminal.rs
- **Heap exhaustion** → silent hang (bump returns null → alloc panics → spin)
- **NVMe init needs CLI** — timer firing mid-MMIO causes triple fault

---

## Crate Dependencies

```toml
limine = "0.6.5"   # Limine boot protocol bindings (MIT)
spin   = "0.9"     # Spinlock Mutex (MIT)
# alloc built from rust-src (MIT/Apache-2)
```

No external OS dependencies. All drivers, FS, and desktop written from scratch.
