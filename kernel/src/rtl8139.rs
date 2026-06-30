//! RTL8139 NIC driver for QEMU.
//! PCI vendor 0x10EC, device 0x8139.
//! Uses I/O BAR0. TX: 4 flat buffers. RX: 64KB ring buffer.

use core::arch::asm;
use crate::{pmm, vmm};

fn inb(p: u16) -> u8  { let v: u8;  unsafe { asm!("in al,dx",  out("al") v,  in("dx") p, options(nomem,nostack)); } v }
fn inw(p: u16) -> u16 { let v: u16; unsafe { asm!("in ax,dx",  out("ax") v,  in("dx") p, options(nomem,nostack)); } v }
fn inl(p: u16) -> u32 { let v: u32; unsafe { asm!("in eax,dx", out("eax") v, in("dx") p, options(nomem,nostack)); } v }
fn outb(p: u16, v: u8)  { unsafe { asm!("out dx,al",  in("dx") p, in("al") v,  options(nomem,nostack)); } }
fn outw(p: u16, v: u16) { unsafe { asm!("out dx,ax",  in("dx") p, in("ax") v,  options(nomem,nostack)); } }
fn outl(p: u16, v: u32) { unsafe { asm!("out dx,eax", in("dx") p, in("eax") v, options(nomem,nostack)); } }

// Register offsets (from I/O base)
const MAC0:    u16 = 0x00; // 6 bytes
const TXADDR0: u16 = 0x20; // TX start address 0-3 (4 × 4B)
const TXSTAT0: u16 = 0x10; // TX status 0-3     (4 × 4B)
const RXBUF:   u16 = 0x30; // RX buffer start (4B)
const CMD:     u16 = 0x37; // command register (1B)
const CAPR:    u16 = 0x38; // current address of packet read (2B)
const CBA:     u16 = 0x3A; // current buffer address (2B, read-only)
const IMR:     u16 = 0x3C; // interrupt mask register (2B)
const ISR:     u16 = 0x3E; // interrupt status register (2B)
const TXCFG:   u16 = 0x40; // TX configuration (4B)
const RXCFG:   u16 = 0x44; // RX configuration (4B)
const CONFIG1: u16 = 0x52; // config register 1 (1B)

// CMD bits
const CMD_TE: u8 = 1 << 2; // TX enable
const CMD_RE: u8 = 1 << 3; // RX enable
const CMD_RST: u8 = 1 << 4;

// TXSTAT bits
const TXSTAT_OWN: u32 = 1 << 13; // OWN bit: 0 = hw owns, 1 = we own
const TXSTAT_TOK: u32 = 1 << 15; // TX OK

// RXCFG: accept all, 64KB ring, no threshold
const RX_ACCEPT_ALL: u32 = 0xF;   // AB|AM|APM|AAP
const RX_WRAP:        u32 = 1 << 7; // WRAP=1 (safer, simpler)
const RX_RBLEN_64K:   u32 = 0b11 << 11;

// RX packet header (first 4 bytes in ring)
const RX_ROK: u16 = 1 << 0; // receive OK

const TX_SLOTS: usize = 4;
const RX_BUF_LEN: usize = 65536 + 16; // 64KB + 16 for overruns

fn dma_page() -> (u64, *mut u8) {
    let p = pmm::alloc_page().expect("rtl8139: OOM");
    let v = vmm::phys_to_virt(p);
    unsafe { core::ptr::write_bytes(v, 0, 4096); }
    (p, v)
}

pub struct Rtl8139 {
    io:      u16,
    pub mac: [u8; 6],
    // TX: 4 flat 2KB slots (all in one page)
    tx_phys: [u64; TX_SLOTS],
    tx_virt: [*mut u8; TX_SLOTS],
    tx_slot: usize,
    // RX: 64KB ring buffer
    rx_phys: u64,
    rx_virt: *mut u8,
    rx_off:  usize, // current read offset in ring
}
unsafe impl Send for Rtl8139 {}

impl Rtl8139 {
    pub fn send(&mut self, data: &[u8]) {
        if data.len() > 1792 { return; }
        let slot = self.tx_slot % TX_SLOTS;
        // Wait for slot to be ours (OWN=1)
        for _ in 0..1_000_000u32 {
            if inl(self.io + TXSTAT0 + slot as u16 * 4) & TXSTAT_OWN != 0 { break; }
            core::hint::spin_loop();
        }
        // Copy packet to TX buffer
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.tx_virt[slot], data.len());
        }
        // Set TX address and length (clears OWN=0 meaning hw owns it)
        outl(self.io + TXADDR0 + slot as u16 * 4, self.tx_phys[slot] as u32);
        outl(self.io + TXSTAT0 + slot as u16 * 4, data.len() as u32 & 0x1FFF);
        self.tx_slot = self.tx_slot.wrapping_add(1);
        // Small delay for QEMU to process
        for _ in 0..50_000u32 { core::hint::spin_loop(); }
    }

    pub fn recv(&mut self) -> Option<alloc::vec::Vec<u8>> {
        // Check if RX has data: CBR (current buffer) != CAPR
        let cbr = inw(self.io + CBA) as usize;
        // CAPR is where we last read, CBR is where hw last wrote
        // We need to check if CBR has advanced past our read offset
        let capr = inw(self.io + CAPR) as usize;
        // Data available when CAPR+16 != CBR (accounting for wrap)
        if (capr + 16) % 65536 == cbr % 65536 { return None; }

        // Read packet header at current offset in ring
        let buf = unsafe { core::slice::from_raw_parts(self.rx_virt, RX_BUF_LEN) };
        let off = self.rx_off;
        let hdr_status = u16::from_le_bytes([buf[off % 65536], buf[(off+1) % 65536]]);
        let pkt_len    = u16::from_le_bytes([buf[(off+2) % 65536], buf[(off+3) % 65536]]) as usize;

        if hdr_status & RX_ROK == 0 || pkt_len < 14 || pkt_len > 1518 {
            // Bad packet — advance past it
            self.rx_off = (off + 4 + pkt_len + 3) & !3;
            outw(self.io + CAPR, (self.rx_off.wrapping_sub(16)) as u16);
            return None;
        }

        // pkt_len includes 4-byte CRC; read pkt_len-4 bytes of actual data
        let frame_len = pkt_len.saturating_sub(4);
        let mut packet = alloc::vec::Vec::with_capacity(frame_len);
        for i in 0..frame_len {
            packet.push(buf[(off + 4 + i) % 65536]);
        }

        // Advance read pointer (4-byte aligned, +16 gap for hardware)
        self.rx_off = (off + 4 + pkt_len + 3) & !3;
        outw(self.io + CAPR, (self.rx_off.wrapping_sub(16)) as u16);

        Some(packet)
    }
}

pub static NIC: spin::Mutex<Option<Rtl8139>> = spin::Mutex::new(None);

pub fn init(devices: &[crate::pci::PciDevice]) {
    use crate::serial;
    for d in devices {
        if d.vendor_id != 0x10EC || d.device_id != 0x8139 { continue; }
        serial::print("rtl8139: found\n");

        // Enable I/O space + bus master
        let cmd = crate::pci::config_read16(d.bus, d.dev, d.func, 0x04);
        crate::pci::config_write32(d.bus, d.dev, d.func, 0x04, (cmd | 0x05) as u32);

        // I/O BAR at BAR0 (bit 0 = 1 = I/O)
        let bar0 = crate::pci::config_read32(d.bus, d.dev, d.func, 0x10);
        if bar0 & 1 == 0 { serial::print("rtl8139: BAR0 not I/O\n"); continue; }
        let io = (bar0 & !3) as u16;
        serial::print_hex("rtl8139: io_base", io as u64);

        // Power on
        outb(io + CONFIG1, 0x00);

        // Software reset
        outb(io + CMD, CMD_RST);
        for _ in 0..100_000u32 {
            if inb(io + CMD) & CMD_RST == 0 { break; }
            core::hint::spin_loop();
        }
        serial::print("rtl8139: reset done\n");

        // Read MAC
        let mut mac = [0u8; 6];
        for i in 0..6 { mac[i] = inb(io + MAC0 + i as u16); }
        serial::print("rtl8139: MAC=");
        for (i,&b) in mac.iter().enumerate() {
            if i>0 { serial::print(":"); }
            serial::print_hex("", b as u64);
        }
        serial::print("\n");

        // Allocate TX buffers (4 × 2KB in 4 separate pages so phys is simple)
        let mut tx_phys = [0u64; TX_SLOTS];
        let mut tx_virt = [core::ptr::null_mut::<u8>(); TX_SLOTS];
        for i in 0..TX_SLOTS {
            let (p, v) = dma_page();
            tx_phys[i] = p;
            tx_virt[i] = v;
            // Mark slot as OWN (bit 13 = 1 means we own it)
            outl(io + TXSTAT0 + i as u16 * 4, TXSTAT_OWN);
        }

        // Allocate RX buffer (64KB + 16 = 2 contiguous pages won't work; use 32 pages)
        // We need 65552 bytes = 17 pages. Use alloc_contiguous.
        let rx_pages = (RX_BUF_LEN + 4095) / 4096; // = 17
        let rx_phys = pmm::alloc_contiguous(rx_pages).expect("rtl8139: rx OOM");
        let rx_virt = vmm::phys_to_virt(rx_phys);
        unsafe { core::ptr::write_bytes(rx_virt, 0, rx_pages * 4096); }
        serial::print_hex("rtl8139: rx_phys", rx_phys);

        // Setup RX buffer
        outl(io + RXBUF, rx_phys as u32);

        // Disable interrupts
        outw(io + IMR, 0);

        // Enable TX+RX
        outb(io + CMD, CMD_TE | CMD_RE);

        // RX config: accept all frames, 64KB buffer, WRAP=1
        outl(io + RXCFG, RX_ACCEPT_ALL | RX_WRAP | RX_RBLEN_64K);

        // TX config: standard gap
        outl(io + TXCFG, 0x03000600);

        // Init CAPR to -16 (wrap trick)
        outw(io + CAPR, 0xFFF0u16);

        serial::print("rtl8139: ready\n");

        *NIC.lock() = Some(Rtl8139 {
            io, mac,
            tx_phys, tx_virt, tx_slot: 0,
            rx_phys, rx_virt, rx_off: 0,
        });
        return;
    }
    serial::print("rtl8139: not found\n");
}
