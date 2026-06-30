//! Intel 82540EM (e1000) Ethernet driver for QEMU.
//! PCI vendor 0x8086, device 0x100E.
//! Uses MMIO BAR0. TX/RX rings of 8 descriptors each.

use core::sync::atomic::{fence, Ordering};
use crate::{pmm, vmm, paging};

// ── e1000 MMIO register offsets ───────────────────────────────────────────────
const CTRL:    usize = 0x0000;
const STATUS:  usize = 0x0008;
const IMS:     usize = 0x00D0; // interrupt mask
const RCTL:    usize = 0x0100; // receive control
const TCTL:    usize = 0x0400; // transmit control
const TIPG:    usize = 0x0410; // TX inter-packet gap
const RDBAL:   usize = 0x2800; // RX desc base lo
const RDBAH:   usize = 0x2804; // RX desc base hi
const RDLEN:   usize = 0x2808; // RX desc ring length (bytes)
const RDH:     usize = 0x2810; // RX desc head
const RDT:     usize = 0x2818; // RX desc tail
const TDBAL:   usize = 0x3800; // TX desc base lo
const TDBAH:   usize = 0x3804;
const TDLEN:   usize = 0x3808;
const TDH:     usize = 0x3810;
const TDT:     usize = 0x3818;
const RAL:     usize = 0x5400; // receive address lo
const RAH:     usize = 0x5404; // receive address hi (+ valid bit)
const MTA:     usize = 0x5200; // multicast table (128 entries)

// CTRL bits
const CTRL_RST:  u32 = 1 << 26;
const CTRL_ASDE: u32 = 1 << 5;
const CTRL_SLU:  u32 = 1 << 6;

// RCTL bits
const RCTL_EN:         u32 = 1 << 1;
const RCTL_UPE:        u32 = 1 << 3;  // unicast promisc
const RCTL_MPE:        u32 = 1 << 4;  // multicast promisc
const RCTL_BAM:        u32 = 1 << 15; // broadcast accept
const RCTL_BSIZE_2048: u32 = 0;
const RCTL_SECRC:      u32 = 1 << 26; // strip CRC

// TCTL bits
const TCTL_EN:   u32 = 1 << 1;
const TCTL_PSP:  u32 = 1 << 3; // pad short packets

// Descriptor status bits
const RX_DD: u8 = 1 << 0; // descriptor done
const TX_DD: u8 = 1 << 0;
const TX_RS: u8 = 1 << 3; // report status
const TX_EOP:u8 = 1 << 0; // end of packet (cmd)

const RING: usize = 8; // TX/RX ring size

// ── RX descriptor (16 bytes) ─────────────────────────────────────────────────
#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
struct RxDesc {
    addr:   u64,
    len:    u16,
    cksum:  u16,
    status: u8,
    errors: u8,
    _spec:  u16,
}

// ── TX descriptor (16 bytes) ─────────────────────────────────────────────────
#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
struct TxDesc {
    addr:   u64,
    len:    u16,
    cso:    u8,
    cmd:    u8,
    status: u8,
    css:    u8,
    _spec:  u16,
}

// ── DMA ring pages ────────────────────────────────────────────────────────────
fn dma_page() -> (u64, *mut u8) {
    let p = pmm::alloc_page().expect("e1000: OOM");
    let v = vmm::phys_to_virt(p);
    unsafe { core::ptr::write_bytes(v, 0, 4096); }
    (p, v)
}

pub struct E1000 {
    regs:   *mut u8,
    pub mac: [u8; 6],
    // RX
    rx_desc:  *mut RxDesc,
    rx_phys:  u64,
    rx_bufs:  [u64; RING],   // physical addresses of RX payload pages
    rx_tail:  usize,
    // TX
    tx_desc:  *mut TxDesc,
    tx_phys:  u64,
    tx_tail:  usize,
}

unsafe impl Send for E1000 {}

impl E1000 {
    fn read32(&self, off: usize) -> u32 {
        unsafe { (self.regs.add(off) as *const u32).read_volatile() }
    }
    fn write32(&self, off: usize, v: u32) {
        unsafe { (self.regs.add(off) as *mut u32).write_volatile(v) }
    }

    pub fn send(&mut self, data: &[u8]) {
        if data.len() > 4096 { return; }
        // Allocate a DMA page for this TX packet
        let (phys, virt) = dma_page();
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), virt, data.len()); }

        let idx = self.tx_tail % RING;
        unsafe {
            (*self.tx_desc.add(idx)) = TxDesc {
                addr:   phys,
                len:    data.len() as u16,
                cmd:    TX_EOP | TX_RS,
                status: 0,
                ..Default::default()
            };
        }
        fence(Ordering::SeqCst);
        self.tx_tail = (self.tx_tail + 1) % RING;
        self.write32(TDT, self.tx_tail as u32);

        // Wait for TX completion
        for _ in 0..2_000_000u32 {
            fence(Ordering::SeqCst);
            let st = unsafe { (*self.tx_desc.add(idx)).status };
            if st & TX_DD != 0 { break; }
            core::hint::spin_loop();
        }
    }

    pub fn recv(&mut self) -> Option<alloc::vec::Vec<u8>> {
        let idx = self.rx_tail % RING;
        fence(Ordering::SeqCst);
        let st = unsafe { (*self.rx_desc.add(idx)).status };
        if st & RX_DD == 0 { return None; }

        let len = unsafe { (*self.rx_desc.add(idx)).len } as usize;
        if len < 14 {
            // Re-arm and skip
            unsafe { (*self.rx_desc.add(idx)).status = 0; }
            self.rx_tail = (self.rx_tail + 1) % RING;
            self.write32(RDT, self.rx_tail as u32);
            return None;
        }

        let virt = vmm::phys_to_virt(self.rx_bufs[idx]);
        let frame = unsafe { core::slice::from_raw_parts(virt, len) };
        let packet = frame.to_vec();

        // Re-arm descriptor
        unsafe { (*self.rx_desc.add(idx)).status = 0; }
        self.rx_tail = (self.rx_tail + 1) % RING;
        self.write32(RDT, self.rx_tail as u32);

        Some(packet)
    }
}

pub static NIC: spin::Mutex<Option<E1000>> = spin::Mutex::new(None);

pub fn init(devices: &[crate::pci::PciDevice]) {
    use crate::serial;

    // Debug: print all PCI vendor/device IDs
    serial::print("PCI scan:\n");
    for d in devices {
        serial::print_hex("  VID", d.vendor_id as u64);
        serial::print_hex("  DID", d.device_id as u64);
    }

    for d in devices {
        // Use pre-parsed vendor_id/device_id from PCI struct
        // Accept all common e1000 variants
        if d.vendor_id != 0x8086 { continue; }
        match d.device_id {
            0x100E | 0x100F | 0x1010 | 0x1011 | 0x1026 | 0x1027 | 0x1028
            | 0x1075 | 0x1076 | 0x1077 | 0x1078 | 0x1079 | 0x107A | 0x107B => {}
            _ => {
                serial::print_hex("  skipping Intel DID", d.device_id as u64);
                continue;
            }
        }

        serial::print("e1000: found\n");

        // Enable memory space + bus master
        let cmd = crate::pci::config_read16(d.bus, d.dev, d.func, 0x04);
        crate::pci::config_write32(d.bus, d.dev, d.func, 0x04, (cmd | 0x06) as u32);

        // Map BAR0 (128KB MMIO)
        let bar0 = crate::pci::config_read32(d.bus, d.dev, d.func, 0x10) as u64;
        let bar_phys = bar0 & !0xF;
        serial::print_hex("e1000: BAR0", bar_phys);
        let regs = paging::map_mmio(bar_phys, 128 * 1024);

        // Reset
        let ctrl = unsafe { (regs.add(CTRL) as *const u32).read_volatile() };
        unsafe { (regs.add(CTRL) as *mut u32).write_volatile(ctrl | CTRL_RST); }
        for _ in 0..10_000 { core::hint::spin_loop(); }
        // Wait for reset to clear
        loop {
            let c = unsafe { (regs.add(CTRL) as *const u32).read_volatile() };
            if c & CTRL_RST == 0 { break; }
        }

        // Link up
        unsafe {
            (regs.add(CTRL) as *mut u32)
                .write_volatile(CTRL_ASDE | CTRL_SLU);
        }

        // Read MAC from RAL/RAH
        let ral = unsafe { (regs.add(RAL) as *const u32).read_volatile() };
        let rah = unsafe { (regs.add(RAH) as *const u32).read_volatile() };
        let mac = [
            (ral & 0xFF) as u8, (ral >> 8 & 0xFF) as u8,
            (ral >> 16 & 0xFF) as u8, (ral >> 24) as u8,
            (rah & 0xFF) as u8, (rah >> 8 & 0xFF) as u8,
        ];
        serial::print("e1000: MAC=");
        for (i, &b) in mac.iter().enumerate() {
            if i > 0 { serial::print(":"); }
            serial::print_hex("", b as u64);
        }
        serial::print("\n");

        // Clear multicast table
        for i in 0..128 {
            unsafe { (regs.add(MTA + i * 4) as *mut u32).write_volatile(0); }
        }

        // Disable interrupts
        unsafe { (regs.add(IMS) as *mut u32).write_volatile(0); }

        // ── RX setup ─────────────────────────────────────────────────────────
        let (rx_phys, rx_virt) = dma_page();
        let rx_desc = rx_virt as *mut RxDesc;
        let mut rx_bufs = [0u64; RING];

        for i in 0..RING {
            let (bp, _) = dma_page();
            rx_bufs[i] = bp;
            unsafe { (*rx_desc.add(i)).addr = bp; }
        }
        unsafe {
            (regs.add(RDBAL) as *mut u32).write_volatile(rx_phys as u32);
            (regs.add(RDBAH) as *mut u32).write_volatile((rx_phys >> 32) as u32);
            (regs.add(RDLEN) as *mut u32)
                .write_volatile((RING * core::mem::size_of::<RxDesc>()) as u32);
            (regs.add(RDH) as *mut u32).write_volatile(0);
            (regs.add(RDT) as *mut u32).write_volatile((RING - 1) as u32);
            (regs.add(RCTL) as *mut u32).write_volatile(
                RCTL_EN | RCTL_UPE | RCTL_MPE | RCTL_BAM | RCTL_SECRC);
        }

        // ── TX setup ─────────────────────────────────────────────────────────
        let (tx_phys, tx_virt) = dma_page();
        let tx_desc = tx_virt as *mut TxDesc;

        // Mark all TX descriptors as done initially
        for i in 0..RING {
            unsafe { (*tx_desc.add(i)).status = TX_DD; }
        }

        unsafe {
            (regs.add(TDBAL) as *mut u32).write_volatile(tx_phys as u32);
            (regs.add(TDBAH) as *mut u32).write_volatile((tx_phys >> 32) as u32);
            (regs.add(TDLEN) as *mut u32)
                .write_volatile((RING * core::mem::size_of::<TxDesc>()) as u32);
            (regs.add(TDH) as *mut u32).write_volatile(0);
            (regs.add(TDT) as *mut u32).write_volatile(0);
            // TIPG: recommended values from e1000 spec
            (regs.add(TIPG) as *mut u32).write_volatile(0x0060200A);
            (regs.add(TCTL) as *mut u32).write_volatile(TCTL_EN | TCTL_PSP | (15 << 4) | (63 << 12));
        }

        serial::print("e1000: ready\n");

        *NIC.lock() = Some(E1000 {
            regs, mac,
            rx_desc, rx_phys,
            rx_bufs, rx_tail: 0,
            tx_desc, tx_phys,
            tx_tail: 0,
        });
        return;
    }
    serial::print("e1000: not found\n");
}
