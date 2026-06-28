#![no_std]
#![no_main]

mod framebuffer;
mod panic;
mod serial;

use limine::request::{FramebufferRequest, HhdmRequest};
use limine::BaseRevision;

#[used]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

#[used]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[no_mangle]
extern "C" fn kmain() -> ! {
    serial::init();
    serial::print("HepOS kmain entered\n");

    if !BASE_REVISION.is_supported() {
        serial::print("WARN: base revision not supported, continuing anyway\n");
    }

    let Some(fb_response) = FRAMEBUFFER_REQUEST.response() else {
        serial::print("ERROR: no framebuffer response from Limine\n");
        loop { core::hint::spin_loop(); }
    };

    serial::print("Got framebuffer response\n");

    let Some(fb) = fb_response.framebuffers().first() else {
        serial::print("ERROR: framebuffer list is empty\n");
        loop { core::hint::spin_loop(); }
    };

    serial::print("Got framebuffer\n");

    let mut display = framebuffer::Display::new(fb);

    display.clear(framebuffer::Color::from_hex(0x0D0D0D));

    let accent = framebuffer::Color::from_hex(0x6C8EFF);
    let white  = framebuffer::Color::from_hex(0xE8E8E8);
    let dim    = framebuffer::Color::from_hex(0x555555);

    display.fill_rect(0, 0, display.width(), 2, accent);

    let x_mid = display.width() / 2;
    let y_mid = display.height() / 2;

    display.draw_text(x_mid - 72, y_mid - 24, "HepOS", accent, 3);
    display.draw_text(x_mid - 88, y_mid + 16, "kernel alive", white, 2);
    display.draw_text(x_mid - 96, y_mid + 48, "v0.1 | x86_64 exokernel", dim, 1);

    serial::print("Framebuffer rendered\n");

    loop {
        core::hint::spin_loop();
    }
}
