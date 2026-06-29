#![no_std]
#![no_main]
#![feature(stmt_expr_attributes)]
extern crate alloc;

mod apic;
mod desktop;
mod framebuffer;
mod gdt;
mod heap;
mod hepfs;
mod idt;
<<<<<<< HEAD
mod mouse;
=======
>>>>>>> 41b662b (add paging and nvme support)
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

    // Init desktop BEFORE enabling interrupts so task_blink sees it immediately
    {
        let fb = FRAMEBUFFER_REQUEST.response()
            .and_then(|r| r.framebuffers().first().copied())
            .expect("no framebuffer for desktop");
        let w = fb.width as usize;
        let h = fb.height as usize;
        let mut dt = desktop::Desktop::new(w, h);
        dt.add_window("Welcome to HepOS", 80,  80,  360, 200);
        dt.add_window("HepFS",            500, 100, 240, 180);
        dt.add_window("Terminal",         200, 320, 400, 220);
        *desktop::DESKTOP.lock() = Some(dt);
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

<<<<<<< HEAD
    // Disable interrupts for entire NVMe + FS init — the timer firing mid-MMIO
    // causes a triple fault because the scheduler switches context before NVMe
    // queue setup is stable.
    unsafe { core::arch::asm!("cli", options(nomem, nostack)); }

=======
>>>>>>> 41b662b (add paging and nvme support)
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
<<<<<<< HEAD

        // HepFS
        if !hepfs::probe(&mut ctrl) {
            serial::print("Formatting HepFS...\n");
            hepfs::format(&mut ctrl);
            serial::print("HepFS formatted\n");
        } else {
            serial::print("HepFS found\n");
        }

        // Smoke test: create dirs + file, write, read back
        hepfs::create_dir(&mut ctrl, hepfs::ROOT_INO, "home");
        hepfs::create_dir(&mut ctrl, hepfs::ROOT_INO, "etc");
        let home = hepfs::lookup(&mut ctrl, "/home").unwrap();
        let fno  = hepfs::create_file(&mut ctrl, home, "hello.txt");
        hepfs::write_file(&mut ctrl, fno, b"Hello from HepOS!\n");
        let data = hepfs::read_file(&mut ctrl, fno);
        let s    = core::str::from_utf8(&data).unwrap_or("?");
        serial::print("Read back: ");
        serial::print(s);

        let entries = hepfs::list_dir(&mut ctrl, hepfs::ROOT_INO);
        serial::print("/ contents:\n");
        for (_, name) in &entries { serial::print("  "); serial::print(name); serial::print("\n"); }

=======
>>>>>>> 41b662b (add paging and nvme support)
    } else {
        serial::print("No NVMe device found\n");
    }

<<<<<<< HEAD
    // Re-enable interrupts now that NVMe + FS are stable
    unsafe { core::arch::asm!("sti", options(nomem, nostack)); }

    // Input devices
=======
    // PS/2 keyboard
>>>>>>> 41b662b (add paging and nvme support)
    ps2::init();
    mouse::init();
    serial::print("Input init\n");

    serial::print("Boot complete\n");

    loop { core::hint::spin_loop(); }
}

fn task_idle() -> ! {
    loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack)); } }
}

fn task_blink() -> ! {
    loop {
        // Poll input
        ps2::poll();
        mouse::poll();

        let (mx, my, btn) = {
            let m = mouse::MOUSE.lock();
            (m.x, m.y, m.buttons)
        };

        // Update window manager with mouse state
        {
            let mut dt_guard = desktop::DESKTOP.lock();
            if let Some(dt) = dt_guard.as_mut() {
                dt.update_mouse(mx, my, btn);
                let w = dt.fb_w;
                let h = dt.fb_h;
                let mut m = mouse::MOUSE.lock();
                m.clamp(w as i32, h as i32);
            }
        }

        // Render desktop only when something changed
        let should_render = desktop::DESKTOP.lock()
            .as_ref().map(|dt| dt.dirty).unwrap_or(false);

        if should_render {
            let mut dt_guard = desktop::DESKTOP.lock();
            if let Some(dt) = dt_guard.as_mut() {
                dt.dirty = false;
                let (cx, cy) = (dt.prev_cx, dt.prev_cy);
                if let Some(display) = DISPLAY.lock().as_mut() {
                    dt.render(display, cx, cy);
                }
            }
        }

        // ~60fps cap
        for _ in 0..1_200_000 { core::hint::spin_loop(); }
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
