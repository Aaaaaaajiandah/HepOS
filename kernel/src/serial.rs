const COM1: u16 = 0x3F8;

pub fn init() {
    unsafe {
        outb(COM1 + 1, 0x00); // disable interrupts
        outb(COM1 + 3, 0x80); // enable DLAB
        outb(COM1 + 0, 0x03); // baud lo: 38400
        outb(COM1 + 1, 0x00); // baud hi
        outb(COM1 + 3, 0x03); // 8 bits, no parity, one stop
        outb(COM1 + 2, 0xC7); // enable FIFO
        outb(COM1 + 4, 0x0B); // enable IRQs, RTS/DSR
    }
}

pub fn print(s: &str) {
    for b in s.bytes() {
        unsafe {
            while inb(COM1 + 5) & 0x20 == 0 {}
            outb(COM1, b);
        }
    }
}

unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
}

unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack));
    val
}
