use core::arch::asm;

fn outw(port: u16, val: u16) {
    unsafe { asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack)); }
}
fn outb(port: u16, val: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack)); }
}

/// Shut down the machine via ACPI.
/// On QEMU: port 0x604 value 0x2000 triggers immediate power-off.
/// On real hardware: would need to parse FADT for PM1a_CNT_BLK address.
pub fn shutdown() -> ! {
    outw(0x604, 0x2000); // QEMU ACPI shutdown
    outw(0xB004, 0x2000); // older QEMU / Bochs fallback
    outw(0x4004, 0x3400); // VirtualBox fallback
    loop { unsafe { asm!("hlt", options(nomem, nostack)); } }
}

/// Reboot via PS/2 controller reset line.
pub fn reboot() -> ! {
    // Pulse reset line via PS/2 controller
    let mut good = 0x02u8;
    while good & 0x02 != 0 {
        good = unsafe {
            let v: u8;
            asm!("in al, dx", out("al") v, in("dx") 0x64u16, options(nomem, nostack));
            v
        };
    }
    outb(0x64, 0xFE);
    loop { unsafe { asm!("hlt", options(nomem, nostack)); } }
}
