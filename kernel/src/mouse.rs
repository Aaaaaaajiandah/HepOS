use core::arch::asm;
use spin::Mutex;

const DATA:   u16 = 0x60;
const STATUS: u16 = 0x64;
const CMD:    u16 = 0x64;

fn inb(p: u16) -> u8 { let v: u8; unsafe { asm!("in al, dx", out("al") v, in("dx") p, options(nomem,nostack)); } v }
fn outb(p: u16, v: u8) { unsafe { asm!("out dx, al", in("dx") p, in("al") v, options(nomem,nostack)); } }
fn wait_w() { while inb(STATUS) & 0x02 != 0 {} }
fn wait_r() { while inb(STATUS) & 0x01 == 0 {} }

fn aux_write(b: u8) {
    wait_w(); outb(CMD, 0xD4);
    wait_w(); outb(DATA, b);
}

pub struct MouseState {
    pub x:       i32,
    pub y:       i32,
    pub buttons: u8,
    cycle:       u8,
    packet:      [u8; 3],
}

impl MouseState {
    const fn new() -> Self {
        Self { x: 640, y: 360, buttons: 0, cycle: 0, packet: [0; 3] }
    }

    pub fn clamp(&mut self, w: i32, h: i32) {
        self.x = self.x.clamp(0, w - 1);
        self.y = self.y.clamp(0, h - 1);
    }
}

pub static MOUSE: Mutex<MouseState> = Mutex::new(MouseState::new());

static BYTE_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

pub fn handle_byte(b: u8) {
    let n = BYTE_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if n < 9 { // print first 9 bytes so we can see if packets arrive
        crate::serial::print_hex("mouse byte", b as u64);
    }
    let mut m = MOUSE.lock();
    match m.cycle {
        0 => {
            if b & 0x08 == 0 { return; } // bit 3 always set in first byte
            m.packet[0] = b;
            m.cycle = 1;
        }
        1 => { m.packet[1] = b; m.cycle = 2; }
        2 => {
            m.packet[2] = b;
            m.cycle = 0;

            let flags = m.packet[0];
            let dx = m.packet[1] as i32 - if flags & 0x10 != 0 { 256 } else { 0 };
            let dy = m.packet[2] as i32 - if flags & 0x20 != 0 { 256 } else { 0 };

            m.x       += dx;
            m.y       -= dy; // Y is inverted on screen
            m.buttons  = flags & 0x07;
        }
        _ => m.cycle = 0,
    }
}

pub fn poll() {
    loop {
        let s = inb(STATUS);
        if s & 0x01 == 0 { break; }   // no data available
        let b = inb(DATA);
        if s & 0x20 != 0 {             // bit 5: data is from aux (mouse)
            handle_byte(b);
        }
        // else: keyboard byte — ps2::poll() handles those
    }
}

fn drain() {
    // Flush any pending bytes without blocking
    for _ in 0..256 {
        if inb(STATUS) & 0x01 == 0 { break; }
        inb(DATA);
    }
}

fn timed_read() -> Option<u8> {
    for _ in 0..100_000u32 {
        if inb(STATUS) & 0x01 != 0 { return Some(inb(DATA)); }
    }
    None
}

fn aux_write_safe(b: u8) {
    wait_w(); outb(CMD, 0xD4);
    wait_w(); outb(DATA, b);
}

pub fn init() {
    drain();

    // Enable auxiliary port
    wait_w(); outb(CMD, 0xA8);
    drain();

    // Update controller config to enable mouse clock
    wait_w(); outb(CMD, 0x20);
    let cfg = timed_read().unwrap_or(0) | 0x02;
    wait_w(); outb(CMD, 0x60);
    wait_w(); outb(DATA, cfg);
    drain();

    // Set defaults — read ACK with timeout, don't hang if mouse absent
    aux_write_safe(0xF6);
    timed_read(); // ACK or None
    drain();

    // Enable data reporting
    aux_write_safe(0xF4);
    timed_read(); // ACK or None
    drain();
}
