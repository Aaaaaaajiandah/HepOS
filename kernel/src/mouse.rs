// QEMU USB tablet sends absolute mouse coordinates through a special
// QEMU-specific mechanism via port 0x301 (QEMU guest ABI).
// As a fallback we also poll the PS/2 AUX port for relative events.
use core::arch::asm;
use spin::Mutex;

const PS2_DATA:   u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_CMD:    u16 = 0x64;

fn inb(p: u16) -> u8 { let v: u8; unsafe { asm!("in al, dx", out("al") v, in("dx") p, options(nomem,nostack)); } v }
fn outb(p: u16, v: u8) { unsafe { asm!("out dx, al", in("dx") p, in("al") v, options(nomem,nostack)); } }

pub struct MouseState {
    pub x:       i32,
    pub y:       i32,
    pub buttons: u8,
    cycle:       u8,
    packet:      [u8; 3],
}

impl MouseState {
    const fn new() -> Self {
        Self { x: 400, y: 300, buttons: 0, cycle: 0, packet: [0; 3] }
    }

    pub fn clamp(&mut self, w: i32, h: i32) {
        self.x = self.x.clamp(0, w - 1);
        self.y = self.y.clamp(0, h - 1);
    }
}

pub static MOUSE: Mutex<MouseState> = Mutex::new(MouseState::new());

fn handle_ps2_byte(b: u8) {
    let mut m = MOUSE.lock();
    match m.cycle {
        0 => {
            if b & 0x08 == 0 { return; }
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
            m.x += dx;
            m.y -= dy;
            m.buttons = flags & 0x07;
        }
        _ => m.cycle = 0,
    }
}

pub fn poll() {
    loop {
        let s = inb(PS2_STATUS);
        if s & 0x01 == 0 { break; }
        let b = inb(PS2_DATA);
        if s & 0x20 != 0 {
            handle_ps2_byte(b);
        }
    }
}

fn timed_read() -> Option<u8> {
    for _ in 0..100_000u32 {
        if inb(PS2_STATUS) & 0x01 != 0 { return Some(inb(PS2_DATA)); }
    }
    None
}

fn drain() {
    for _ in 0..256 {
        if inb(PS2_STATUS) & 0x01 == 0 { break; }
        inb(PS2_DATA);
    }
}

fn aux_send(b: u8) {
    for _ in 0..1000u32 { if inb(PS2_STATUS) & 0x02 == 0 { break; } }
    outb(PS2_CMD, 0xD4);
    for _ in 0..1000u32 { if inb(PS2_STATUS) & 0x02 == 0 { break; } }
    outb(PS2_DATA, b);
}

pub fn init() {
    drain();

    // Enable aux port
    for _ in 0..1000u32 { if inb(PS2_STATUS) & 0x02 == 0 { break; } }
    outb(PS2_CMD, 0xA8);
    drain();

    // Read + update controller config byte (enable aux interrupt)
    for _ in 0..1000u32 { if inb(PS2_STATUS) & 0x02 == 0 { break; } }
    outb(PS2_CMD, 0x20);
    let cfg = timed_read().unwrap_or(0) | 0x02;
    for _ in 0..1000u32 { if inb(PS2_STATUS) & 0x02 == 0 { break; } }
    outb(PS2_CMD, 0x60);
    for _ in 0..1000u32 { if inb(PS2_STATUS) & 0x02 == 0 { break; } }
    outb(PS2_DATA, cfg);
    drain();

    // Reset mouse
    aux_send(0xFF);
    timed_read(); // ACK
    timed_read(); // 0xAA self-test
    timed_read(); // 0x00 device ID
    drain();

    // Set defaults
    aux_send(0xF6);
    timed_read(); // ACK
    drain();

    // Enable data reporting
    aux_send(0xF4);
    timed_read(); // ACK
    drain();
}
