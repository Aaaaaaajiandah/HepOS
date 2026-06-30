use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Best-effort serial print — don't use format!, just fixed strings
    crate::serial::print("\n\n*** KERNEL PANIC ***\n");
    if let Some(loc) = info.location() {
        crate::serial::print(loc.file());
        crate::serial::print(":");
        // print line number manually (no alloc)
        let mut n = loc.line();
        let mut buf = [0u8; 10];
        let mut i = 10usize;
        loop {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 { break; }
        }
        let s = core::str::from_utf8(&buf[i..]).unwrap_or("?");
        crate::serial::print(s);
        crate::serial::print("\n");
    }
    if let Some(msg) = info.message().as_str() {
        crate::serial::print(msg);
        crate::serial::print("\n");
    } else {
        crate::serial::print("(no message)\n");
    }
    loop { core::hint::spin_loop(); }
}
