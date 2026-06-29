#![no_std]
#![no_main]
#![feature(stmt_expr_attributes)]
extern crate alloc;

mod apic;
mod framebuffer;
mod gdt;
mod heap;
mod idt;
mod nvme;
mod paging;
mod panic;
mod pci;
mod pmm;
mod ps2;
mod scheduler;
mod serial;
mod vmm;

use framebuffer::Display;
use limine::request::{FramebufferRequest, HhdmRequest};
use limine::BaseRevision;
use spin::Mutex;

// Global display — used by exception handler and future modules
pub static DISPLAY: Mutex<Option<Display>> = Mutex::new(None);

#[used] static BASE_REVISION:       BaseRevision       = BaseRevision::new();
#[used] static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();
#[used] static HHDM_REQUEST:        HhdmRequest        = HhdmRequest::new();

#[no_mangle]
extern "C" fn kmain() -> ! {
    serial::init();
    serial::print("HepOS kmain\n");

    gdt::init();
    serial::print("GDT loaded\n");

    idt::init();
    serial::print("IDT loaded\n");

    let hhdm = HHDM_REQUEST.response().expect("no HHDM").offset;
    vmm::init(hhdm);
    pmm::init(hhdm);
    serial::print("PMM init\n");

    heap::HEAP.init();
    serial::print("Heap init\n");

    // smoke test: allocate and use a Vec
    {
        use alloc::vec::Vec;
        let mut v: Vec<u32> = Vec::new();
        for i in 0..16 { v.push(i); }
        serial::print("Heap smoke test OK\n");
        let _ = v;
    }

    let fb = FRAMEBUFFER_REQUEST
        .response()
        .and_then(|r| r.framebuffers().first().copied())
        .expect("no framebuffer");

    *DISPLAY.lock() = Some(Display::new(fb));

    {
        let mut guard = DISPLAY.lock();
        let display = guard.as_mut().unwrap();

        display.clear(framebuffer::Color::from_hex(0x0D0D0D));

        let accent = framebuffer::Color::from_hex(0x6C8EFF);
        let white  = framebuffer::Color::from_hex(0xE8E8E8);
        let dim    = framebuffer::Color::from_hex(0x555555);

        display.fill_rect(0, 0, display.width(), 2, accent);

        let x_mid = display.width() / 2;
        let y_mid = display.height() / 2;

        display.draw_text(x_mid - 72, y_mid - 24, "HepOS",               accent, 3);
        display.draw_text(x_mid - 88, y_mid + 16, "kernel alive",         white,  2);
        display.draw_text(x_mid - 96, y_mid + 48, "v0.1 | x86_64 exokernel", dim, 1);

        // show memory stats
        let free_mb  = pmm::free_pages()  * 4 / 1024;
        let total_mb = pmm::total_pages() * 4 / 1024;
        let mut buf = [0u8; 64];
        let mem_str = fmt_mem(free_mb, total_mb, &mut buf);
        display.draw_text(x_mid - (mem_str.len() * 9 / 2), y_mid + 72, mem_str, dim, 1);
    }

    // Wire timer interrupt into IDT
    idt::set_handler(apic::timer_vector(), idt::timer_stub as u64);

    apic::init();
    serial::print("APIC init\n");

    // Add two tasks so the scheduler has something to switch between
    {
        let mut sched = scheduler::SCHEDULER.lock();
        sched.add(scheduler::Task::new(0, "idle",  task_idle));
        sched.add(scheduler::Task::new(1, "blink", task_blink));
        sched.tasks[0].state = scheduler::TaskState::Running;
    }

    serial::print("Scheduler ready\n");

    // Enable interrupts — APIC timer will now fire
    unsafe { core::arch::asm!("sti", options(nomem, nostack)); }

    // PCI enumeration
    let pci_devices = pci::enumerate();
    serial::print("PCI devices:\n");
    for d in &pci_devices {
        serial::print("  ");
        serial::print(pci::class_name(d.class, d.subclass));
        serial::print("\n");
    }

    // NVMe
    if let Some(mut ctrl) = nvme::init(&pci_devices) {
        serial::print("NVMe ready\n");
        let s = alloc::format!(
            "NVMe: {} MB  ({} byte blocks)\n",
            ctrl.lba_count * ctrl.lba_size as u64 / 1024 / 1024,
            ctrl.lba_size
        );
        serial::print(&s);
        // smoke test: write then read block 0
        let (phys, virt) = {
            let p = pmm::alloc_page().unwrap();
            (p, vmm::phys_to_virt(p))
        };
        unsafe { core::ptr::write_bytes(virt, 0xAB, 512); }
        ctrl.write_blocks(0, 1, phys);
        unsafe { core::ptr::write_bytes(virt, 0x00, 512); }
        ctrl.read_blocks(0, 1, phys);
        let ok = unsafe { *(virt as *const u8) } == 0xAB;
        serial::print(if ok { "NVMe R/W OK\n" } else { "NVMe R/W FAIL\n" });
    } else {
        serial::print("No NVMe device found\n");
    }

    // PS/2 keyboard
    ps2::init();
    serial::print("PS/2 init\n");

    serial::print("Boot complete\n");

    loop { core::hint::spin_loop(); }
}

fn task_idle() -> ! {
    loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack)); } }
}

fn task_blink() -> ! {
    let white  = framebuffer::Color::from_hex(0xE8E8E8);
    let bg     = framebuffer::Color::from_hex(0x0D0D0D);
    let cursor = framebuffer::Color::from_hex(0x6C8EFF);
    let mut col: usize = 0;
    let row: usize = 8; // text row in chars
    loop {
        // Poll PS/2 (will use interrupt later)
        ps2::poll();
        if let Some(c) = ps2::read_char() {
            if let Some(d) = DISPLAY.lock().as_mut() {
                if c == '\n' || col > 60 {
                    col = 0;
                } else {
                    // erase cursor
                    d.fill_rect(col * 9 + 24, row * 18, 8, 16, bg);
                    // draw char
                    let s: &[u8] = &[c as u8];
                    let text = core::str::from_utf8(s).unwrap_or("?");
                    d.draw_text(col * 9 + 24, row * 18, text, white, 1);
                    col += 1;
                    // draw cursor
                    d.fill_rect(col * 9 + 24, row * 18 + 14, 8, 2, cursor);
                }
            }
        }
        core::hint::spin_loop();
    }
}

fn fmt_mem<'a>(free_mb: u64, total_mb: u64, buf: &'a mut [u8; 64]) -> &'a str {
    let mut pos = 0usize;
    for b in b"RAM: "       { if pos < 64 { buf[pos] = *b; pos += 1; } }
    write_u64(free_mb,  &mut pos, buf);
    for b in b" MB free / " { if pos < 64 { buf[pos] = *b; pos += 1; } }
    write_u64(total_mb, &mut pos, buf);
    for b in b" MB total"   { if pos < 64 { buf[pos] = *b; pos += 1; } }
    core::str::from_utf8(&buf[..pos]).unwrap_or("")
}

fn write_u64(mut n: u64, pos: &mut usize, buf: &mut [u8; 64]) {
    if n == 0 { if *pos < 64 { buf[*pos] = b'0'; *pos += 1; } return; }
    let start = *pos;
    while n > 0 {
        if *pos < 64 { buf[*pos] = b'0' + (n % 10) as u8; *pos += 1; }
        n /= 10;
    }
    buf[start..*pos].reverse();
}
