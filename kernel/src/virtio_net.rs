//! Virtio-net legacy PCI driver (correct 2-page queue layout).
//!
//! Legacy virtio queue layout (queue size Q, 4096-byte alignment):
//!   Page 1: Descriptor table (Q * 16 B) + Available ring (4 + Q*2 B)
//!   Page 2: Used ring (4 + Q*8 B)   ← must be at phys_page1 + 4096

use core::{arch::asm, sync::atomic::{fence, Ordering}};
use crate::{pmm, vmm};

// ── I/O helpers ──────────────────────────────────────────────────────────────
fn inb(p: u16) -> u8  { let v: u8;  unsafe { asm!("in al,dx",  out("al") v,  in("dx") p, options(nomem,nostack)); } v }
fn inw(p: u16) -> u16 { let v: u16; unsafe { asm!("in ax,dx",  out("ax") v,  in("dx") p, options(nomem,nostack)); } v }
fn inl(p: u16) -> u32 { let v: u32; unsafe { asm!("in eax,dx", out("eax") v, in("dx") p, options(nomem,nostack)); } v }
fn outb(p: u16, v: u8)  { unsafe { asm!("out dx,al",  in("dx") p, in("al") v,  options(nomem,nostack)); } }
fn outw(p: u16, v: u16) { unsafe { asm!("out dx,ax",  in("dx") p, in("ax") v,  options(nomem,nostack)); } }
fn outl(p: u16, v: u32) { unsafe { asm!("out dx,eax", in("dx") p, in("eax") v, options(nomem,nostack)); } }

// ── Virtio register offsets ───────────────────────────────────────────────────
const DEV_FEAT: u16 = 0x00;
const DRV_FEAT: u16 = 0x04;
const Q_PFN:    u16 = 0x08;
const Q_SIZE:   u16 = 0x0C;
const Q_SEL:    u16 = 0x0E;
const Q_NOTIFY: u16 = 0x10;
const DEV_STAT: u16 = 0x12;
const NET_MAC:  u16 = 0x14;

const VRING_DESC_F_NEXT:  u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

// ── Queue depth (must fit desc+avail in one 4096-byte page) ──────────────────
// For Q=64: desc=1024B, avail=132B → total=1156B → fits in 4096B ✓
pub const Q: usize = 64;

// ── On-disk / DMA structures ─────────────────────────────────────────────────
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Desc { addr: u64, len: u32, flags: u16, next: u16 }

#[repr(C)]
struct Avail { flags: u16, idx: u16, ring: [u16; Q] }

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UsedElem { id: u32, len: u32 }

#[repr(C)]
struct Used { flags: u16, idx: u16, ring: [UsedElem; Q] }

// Virtio-net header (10 bytes, all zeros for basic operation)
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct NetHdr {
    flags: u8, gso_type: u8, hdr_len: u16, gso_size: u16,
    csum_start: u16, csum_offset: u16,
}

// ── Alloc helpers ─────────────────────────────────────────────────────────────
fn dma_page() -> (u64, *mut u8) {
    let p = pmm::alloc_page().expect("virtio: OOM");
    let v = vmm::phys_to_virt(p);
    unsafe { core::ptr::write_bytes(v, 0, 4096); }
    (p, v)
}

fn dma_pages_2() -> (u64, *mut u8, u64, *mut u8) {
    // Allocate 2 physically contiguous pages (required for virtqueue layout)
    let base = pmm::alloc_contiguous(2).expect("virtio: need 2 contiguous pages");
    let v1 = vmm::phys_to_virt(base);
    let v2 = vmm::phys_to_virt(base + 4096);
    unsafe {
        core::ptr::write_bytes(v1, 0, 4096);
        core::ptr::write_bytes(v2, 0, 4096);
    }
    (base, v1, base + 4096, v2)
}

// ── Virtqueue ────────────────────────────────────────────────────────────────
struct VQ {
    // DMA memory
    page1_phys: u64,         // PFN to tell host
    desc:  *mut Desc,        // page1 offset 0
    avail: *mut Avail,       // page1 offset Q*16
    used:  *mut Used,        // page2 (page1 + 4096)
    // Driver state
    free_head: usize,        // next free desc slot
    avail_idx: u16,          // # entries put in avail ring (driver)
    last_used: u16,          // last used idx we've processed
    // Per-slot payload buffers
    bufs: [u64; Q],          // physical addresses
}
unsafe impl Send for VQ {}

impl VQ {
    fn new() -> Self {
        let (p1, v1, _p2, v2) = dma_pages_2();
        crate::serial::print_hex("virtio: queue p1", p1);
        VQ {
            page1_phys: p1,
            desc:  v1 as *mut Desc,
            avail: unsafe { v1.add(Q * 16) as *mut Avail },
            used:  v2 as *mut Used,
            free_head: 0,
            avail_idx: 0,
            last_used: 0,
            bufs: [0; Q],
        }
    }

    /// Pre-fill queue with `n` device-writable buffers (for RX).
    fn prefill_rx(&mut self, n: usize) {
        for i in 0..n.min(Q) {
            let (phys, _) = dma_page();
            self.bufs[i] = phys;
            unsafe {
                (*self.desc.add(i)) = Desc {
                    addr:  phys,
                    len:   4096,
                    flags: VRING_DESC_F_WRITE,
                    next:  0,
                };
                (*self.avail).ring[i] = i as u16;
            }
        }
        fence(Ordering::SeqCst);
        unsafe { (*self.avail).idx = n as u16; }
        self.avail_idx = n as u16;
        self.free_head = n;
    }

    /// Add a single driver-readable buffer (for TX).
    fn add_tx(&mut self, phys: u64, len: u32) {
        let slot = self.free_head % Q;
        unsafe {
            (*self.desc.add(slot)) = Desc { addr: phys, len, flags: 0, next: 0 };
            let ai = (*self.avail).idx as usize % Q;
            (*self.avail).ring[ai] = slot as u16;
            fence(Ordering::SeqCst);
            (*self.avail).idx = (*self.avail).idx.wrapping_add(1);
        }
        self.free_head = self.free_head.wrapping_add(1);
        self.avail_idx = self.avail_idx.wrapping_add(1);
    }

    /// Poll used ring; returns (desc_idx, written_bytes) if host completed a buffer.
    fn pop_used(&mut self) -> Option<(usize, u32)> {
        fence(Ordering::SeqCst);
        let host_idx = unsafe { (*self.used).idx };
        if host_idx == self.last_used { return None; }
        let elem = unsafe { (*self.used).ring[self.last_used as usize % Q] };
        self.last_used = self.last_used.wrapping_add(1);
        Some((elem.id as usize, elem.len))
    }

    /// Re-add a consumed RX buffer back to the available ring.
    fn recycle_rx(&mut self, slot: usize) {
        unsafe {
            (*self.desc.add(slot)) = Desc {
                addr: self.bufs[slot], len: 4096,
                flags: VRING_DESC_F_WRITE, next: 0,
            };
            let ai = (*self.avail).idx as usize % Q;
            (*self.avail).ring[ai] = slot as u16;
            fence(Ordering::SeqCst);
            (*self.avail).idx = (*self.avail).idx.wrapping_add(1);
        }
    }
}

// ── VirtioNet ────────────────────────────────────────────────────────────────
pub struct VirtioNet {
    io: u16,
    pub mac: [u8; 6],
    rx: VQ,
    tx: VQ,
}
unsafe impl Send for VirtioNet {}

impl VirtioNet {
    fn init_queue(&self, index: u16, vq: &VQ) {
        outw(self.io + Q_SEL, index);
        let _size = inw(self.io + Q_SIZE); // read queue size (informational)
        outl(self.io + Q_PFN, (vq.page1_phys >> 12) as u32);
    }

    pub fn send(&mut self, data: &[u8]) {
        // Prepend virtio-net header (10 zero bytes) + frame in one page
        let (phys, virt) = dma_page();
        let hdr_sz = core::mem::size_of::<NetHdr>();
        unsafe {
            core::ptr::write_bytes(virt, 0, hdr_sz);
            core::ptr::copy_nonoverlapping(data.as_ptr(), virt.add(hdr_sz), data.len());
        }
        self.tx.add_tx(phys, (hdr_sz + data.len()) as u32);
        outw(self.io + Q_NOTIFY, 1); // notify TX queue (index 1)
        // Wait for TX completion
        for _ in 0..2_000_000u32 {
            if self.tx.pop_used().is_some() { break; }
            core::hint::spin_loop();
        }
    }

    pub fn recv(&mut self) -> Option<alloc::vec::Vec<u8>> {
        if let Some((slot, len)) = self.rx.pop_used() {
            let hdr_sz = core::mem::size_of::<NetHdr>();
            let frame_len = (len as usize).saturating_sub(hdr_sz);
            if frame_len == 0 {
                self.rx.recycle_rx(slot);
                outw(self.io + Q_NOTIFY, 0);
                return None;
            }
            let virt = vmm::phys_to_virt(self.rx.bufs[slot]);
            let frame = unsafe {
                core::slice::from_raw_parts(virt.add(hdr_sz), frame_len)
            };
            let packet = frame.to_vec();
            self.rx.recycle_rx(slot);
            outw(self.io + Q_NOTIFY, 0); // notify RX queue (index 0)
            return Some(packet);
        }
        None
    }
}

pub static NIC: spin::Mutex<Option<VirtioNet>> = spin::Mutex::new(None);

pub fn init(devices: &[crate::pci::PciDevice]) {
    use crate::serial;
    for d in devices {
        if d.class != 0x02 || d.subclass != 0x00 { continue; }
        let vid = crate::pci::config_read16(d.bus, d.dev, d.func, 0x00);
        if vid != 0x1AF4 { continue; }

        // Enable I/O space + bus master
        let cmd = crate::pci::config_read16(d.bus, d.dev, d.func, 0x04);
        crate::pci::config_write32(d.bus, d.dev, d.func, 0x04, (cmd | 0x05) as u32);

        let bar0 = crate::pci::config_read32(d.bus, d.dev, d.func, 0x10);
        if bar0 & 1 == 0 { serial::print("virtio: BAR0 not I/O\n"); continue; }
        let io = (bar0 & !3) as u16;

        // Reset → ACKNOWLEDGE → DRIVER
        outb(io + DEV_STAT, 0);
        outb(io + DEV_STAT, 1);
        outb(io + DEV_STAT, 3);

        // Accept all offered features
        let feat = inl(io + DEV_FEAT);
        outl(io + DRV_FEAT, feat);

        // Read MAC
        let mut mac = [0u8; 6];
        for i in 0..6 { mac[i] = inb(io + NET_MAC + i as u16); }

        let mut nic = VirtioNet { io, mac, rx: VQ::new(), tx: VQ::new() };

        // Check queue size
        outw(io + Q_SEL, 0);
        let qs = inw(io + Q_SIZE) as usize;
        serial::print_hex("virtio: queue size", qs as u64);

        // Pre-fill RX queue with Q buffers
        nic.rx.prefill_rx(Q.min(qs));
        nic.init_queue(0, &nic.rx);
        nic.init_queue(1, &nic.tx);

        // DRIVER_OK
        outb(io + DEV_STAT, 7);

        serial::print("virtio-net: ready  MAC=");
        for (i, &b) in mac.iter().enumerate() {
            if i > 0 { serial::print(":"); }
            let mut buf = [0u8; 3];
            buf[0] = b"0123456789ABCDEF"[(b >> 4) as usize];
            buf[1] = b"0123456789ABCDEF"[(b & 0xF) as usize];
            serial::print(core::str::from_utf8(&buf[..2]).unwrap_or("??"));
        }
        serial::print("\n");

        *NIC.lock() = Some(nic);
        return;
    }
    serial::print("virtio-net: not found\n");
}
