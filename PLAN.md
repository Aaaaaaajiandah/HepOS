# HepOS — Complete Design Reference

> **Purpose:** Authoritative reference for HepOS. Survives context compaction.
> **Last updated:** 2026-06-29

---

## Overview

HepOS is a custom x86\_64 operating system written in Rust using an **exokernel architecture**. The kernel only does hardware multiplexing; all OS abstractions live in a kernel-space libOS for now. Single user, no permissions, networking partially implemented.

**Language:** Rust (nightly, `no_std` + `alloc`)  
**Target:** x86\_64, bare metal  
**Bootloader:** Limine v9.x (BIOS mode)  
**Dev machine:** Windows 11, QEMU 11.x  
**License:** MIT  
**Repository:** https://github.com/The-Hep-Group/HepOS

---

## Source Files

```
kernel/src/
  main.rs        kmain entry, global state, task setup, window rendering
  framebuffer.rs GOP pixel/rect/text renderer (8×8 bitmap font, LSB-first)
  gdt.rs         GDT (null, code64, data64)
  idt.rs         IDT, exception stubs, timer_stub
  pmm.rs         Bitmap PMM (pages above 1MB only, alloc_contiguous)
  vmm.rs         HHDM offset, phys_to_virt
  paging.rs      PML4 walker, map_page, map_mmio (NOCACHE)
  heap.rs        Bump allocator 1MB, GlobalAlloc, no-free
  apic.rs        x2APIC (MSR), 10ms timer, disables 8259 PIC
  acpi.rs        ACPI shutdown (port 0x604) + PS/2 reboot
  rtc.rs         CMOS RTC: now(), fmt_time(), fmt_date()
  scheduler.rs   Round-robin preemptive, context_switch (naked asm)
  pci.rs         Config-space scan (0xCF8/0xCFC), enumerate()
  ps2.rs         PS/2 kbd: scancode set 1 + extended (0xE0) + shift/caps/ctrl
  mouse.rs       PS/2 AUX mouse (3-byte packets, relative, AUX port)
  nvme.rs        NVMe driver, admin+IO queues, global CONTROLLER
  hepfs.rs       HepFS: flat inode, 4KB blocks, NVMe backend
  desktop.rs     Compositor, WM, taskbar, RTC clock, dirty-rect
  terminal.rs    Full shell: pwd/cd/ls/cat/mkdir/rm/write/edit/ping/...
  editor.rs      Text editor: arrow nav, F2=save, F10=close
  net.rs         ARP, ICMP, eth_send, ping (bypasses ARP, uses SLiRP MAC)
  e1000.rs       Intel 82540EM driver (TX works, RX pending)
  rtl8139.rs     RTL8139 driver (simpler, flat ring, added)
  virtio_net.rs  virtio-net legacy (incomplete)
  serial.rs      COM1 debug: print, print_hex
  panic.rs       Spins forever
```

---

## QEMU Command

```powershell
qemu-system-x86_64
  -M q35
  -cpu qemu64,+x2apic      # x2APIC via MSR
  -m 256M
  -cdrom hepos.iso
  -boot d
  -drive file=hepos_disk.img,if=none,id=nvme0,format=raw
  -device nvme,serial=heposv1,drive=nvme0
  -netdev user,id=net0
  -device rtl8139,netdev=net0   # RTL8139 NIC (simpler than e1000)
  -vga std
  -display sdl,window-close=off
  -serial stdio
  -no-reboot
  -no-shutdown
```

---

## Boot Sequence

```
Limine → kmain()
 1. serial, GDT, IDT
 2. VMM (HHDM offset), PMM (pages >1MB)
 3. Heap (bump, 256 PMM pages = 1MB)
 4. Display + splash screen
 5. Desktop + windows created (before sti)
 6. Terminal init (before sti)
 7. APIC timer (x2APIC, 10ms tick)
 8. Scheduler (2 tasks: idle, render)
 9. sti → interrupts enabled
10. PCI enumerate
11. CLI
12. NVMe init → HepFS mount/format → write /kernel.txt
13. Networking init (RTL8139 → e1000 → virtio fallbacks)
14. sti
15. PS/2 keyboard + mouse
16. loop { spin_loop }   ← render task (task_blink) does all work
```

---

## Focus System

- **Default:** Terminal focused, all keys → terminal
- **ESC:** Enter cursor mode (yellow crosshair, WASD moves)
- **ESC in cursor mode:** closes editor window, refocuses terminal
- **Space on window (cursor mode):** focus that window
- **ESC from editor:** close editor → terminal
- **Ctrl+C in terminal:** cancel input, show ^C

---

## Desktop Windows (IDs)

| ID | Title | Content |
|----|-------|---------|
| 0 | Welcome to HepOS | System info, RAM, NVMe/HepFS status |
| 1 | HepFS | Live directory listing |
| 2 | Terminal | Full interactive shell |
| 3 | Editor | Text editor (opens with `edit <file>`) |

---

## Terminal Shell Commands

| Command | Description |
|---------|-------------|
| `help` | List commands |
| `pwd` | Print working dir |
| `ls [path]` | List directory |
| `cd <dir>` | Change dir (`..` and `/` supported) |
| `cat <file>` | Print file |
| `mkdir/touch/rm <name>` | Create/delete |
| `write <file> <text>` | Write text to file |
| `edit <file>` | Open text editor |
| `history` | Command history |
| `↑/↓` arrows | Navigate history (Ctrl+P/N also work) |
| `date` | Current date+time (RTC) |
| `sysinfo` | Full kernel info |
| `uname/mem` | System info |
| `lspci` | List all PCI devices |
| `netdiag` | e1000 register dump |
| `netstart` | Force-init e1000 NIC |
| `netpoll` | Scan RX descriptors |
| `ifconfig` | Show IP/MAC/GW |
| `ping <ip>` | ICMP echo (uses hardcoded SLiRP MAC) |
| `shutdown/reboot` | ACPI power off / PS/2 reset |
| `echo/clear` | Print / clear |

---

## HepFS Layout

```
Block 0      : Superblock (magic 0x48657046_53000001)
Block 1      : Inode bitmap
Blocks 2-5   : Block bitmap
Blocks 6-37  : Inode table (1024 inodes × 128 bytes)
Blocks 38+   : Data blocks (4KB each)
```
Max file size: 12 × 4KB = 49KB (direct blocks only)  
/kernel.txt written at every boot (kernel manifest)

---

## Networking Status

**Architecture:** Ethernet → ARP/IP → ICMP  
**Stack:** hand-written (no smoltcp), in `net.rs`  
**Static IP:** 10.0.2.15, GW: 10.0.2.2, mask: 255.255.255.0

### Drivers tried (all for QEMU Windows):

| Driver | Status | Notes |
|--------|--------|-------|
| virtio-net | ✗ not found | PCI vendor 0x1AF4 not detected |
| e1000 (82540EM) | ⚠ TX only | TX processed (TDH advances), pcap shows nothing, SLiRP not responding |
| rtl8139 | ⚠ just added | Simpler flat-buffer design, awaiting test |

**Root cause:** QEMU on Windows with SLiRP — TX data is correct (verified MACs/sizes) but packets don't reach SLiRP netdev. Works on Linux/KVM.

**Workaround:** `netstart` force-inits e1000 manually (NIC = Some). `ping 10.0.2.2` shows "timeout" (not "no route"), meaning TX works.

**Next step for networking:** Test on Linux/KVM, or investigate QEMU Windows SLiRP path.

---

## What's Built ✓ vs Left ○

### Kernel
| | |
|-|-|
| ✓ Boot/Limine, Framebuffer, GDT, IDT | |
| ✓ PMM (>1MB only), HHDM, Paging | |
| ✓ Bump heap, x2APIC, ACPI, RTC | |
| ✓ Preemptive scheduler, PCI enum | |
| ○ Syscall interface (SYSCALL/SYSRET) | |
| ○ Per-process paging, TSS | |

### Input/Drivers
| | |
|-|-|
| ✓ PS/2 keyboard (shift, caps, ctrl, arrow keys, F-keys) | |
| ✓ PS/2 mouse (driver works; QEMU SDL doesn't route to AUX) | |
| ✓ NVMe (admin+IO queues, 512B blocks hardcoded) | |
| ✓ PCI enumeration | |
| ○ XHCI → real mouse on hardware | |
| ○ USB HID | |
| ○ Intel HDA audio | |
| ○ RTL8169 (user's real NIC) | |
| ○ ACPI full (FADT parsing for real hardware) | |

### Storage
| | |
|-|-|
| ✓ HepFS: format, probe, create/read/write/delete files+dirs | |
| ✓ Path resolution, kernel manifest | |
| ○ File deletion frees blocks properly (basic impl) | |
| ○ Large files >49KB (indirect blocks) | |
| ○ VFS layer | |

### Desktop
| | |
|-|-|
| ✓ Compositor (dirty rects, z-order rendering) | |
| ✓ Floating WM (drag, minimize, focus) | |
| ✓ Taskbar + live RTC clock | |
| ✓ Focus system (ESC/space cursor mode) | |
| ✓ Window content rendering (terminal, HepFS, welcome, editor) | |
| ○ Real mouse (needs XHCI) | |
| ○ Animations, desktop icons | |

### Apps
| | |
|-|-|
| ✓ Terminal: full shell with history, arrow nav, Ctrl+C | |
| ✓ Text editor: arrow nav, F2=save, F10=close, insert/delete | |
| ✓ HepFS file manager (basic, list in window) | |
| ○ Image viewer, media player, settings | |

### Networking
| | |
|-|-|
| ✓ ARP, ICMP, IP checksum, eth_send | |
| ✓ e1000 TX works (TDH advances, correct data) | |
| ⚠ e1000 RX not working on QEMU Windows | |
| ⚠ RTL8139 driver written (simpler approach) but still doesnt work properly | |
| ○ Actual ping working | |
| ○ TCP/UDP, DNS, HTTP | |

### File Formats / LibOS
| | |
|-|-|
| ○ PNG/JPG (image crate after std shim) | |
| ○ MP3/FLAC (Symphonia) | |
| ○ std shim for Rust crates | |

---

## Known Issues

| Issue | Cause | Fix |
|-------|-------|-----|
| Mouse doesn't move | QEMU SDL routes to absolute ptr not PS/2 AUX | XHCI + USB HID |
| Networking RX broken on QEMU Win | SLiRP/e1000 path issue, QEMU-Windows-specific | Linux/KVM or RTL8139 |
| Heap can't free | Bump allocator | Slab allocator later |
| NVMe disk size = 0 MB | Identify Namespace hangs | Hardcoded 512B |
| Files max 49KB | Only 12 direct blocks | Indirect blocks |
| ACPI only on QEMU | Hardcoded port 0x604 | Parse FADT |

---

## QEMU Hardware Details

| Item | Value |
|------|-------|
| RAM | 256MB |
| NVMe | 512MB raw (`hepos_disk.img`, auto-created) |
| NVMe BAR | 0xFEBD4000 |
| e1000 BAR | 0xFEBC0000 |
| e1000 MAC | 52:54:00:12:34:56 |
| SLiRP gateway | 10.0.2.2, MAC 52:55:0a:00:02:02 |
| HHDM offset | 0xFFFF800000000000 (typical) |

---

## Next Steps (Priority Order)

1. **XHCI USB** → real mouse (critical for hardware use)
2. **RTL8139 networking** → confirm on real hardware or Linux
3. **Text editor polish** → status bar, line highlight
4. **std shim** → unlock Rust ecosystem (image, audio crates)
5. **Symphonia** → MP3/FLAC audio
6. **RTL8169** → user's actual NIC on real hardware
7. **Syscall/userspace** → ring 3 processes

---

## Dev Tips

- **Serial output** → PowerShell terminal (early boot messages scroll past)
- **ESC** → cursor mode (yellow +); again → terminal focus
- **WASD in cursor mode** → move cursor
- **Space on window** → focus it
- **Ctrl+L** in terminal → clear
- **↑/↓** → history; **Ctrl+P/N** → history (reliable alternative)
- **F2** = editor save, **F10** = editor close
- **`netstart`** → manually init e1000 (workaround for auto-init failure)
- **Heap exhaustion** → silent hang (bump returns null → panic → spin)
- **`lspci`** → see all PCI devices with vendor:device IDs
- **`sysinfo`** / **`cat /kernel.txt`** → kernel info from inside OS

---

## Crate Dependencies

```toml
limine = "0.6.5"   # MIT
spin   = "0.9"     # MIT
# alloc from rust-src (MIT/Apache-2)
```

All drivers, FS, networking, and desktop written from scratch.
