//! Minimal XHCI host controller driver — QEMU `qemu-xhci` + `usb-tablet`.
//! Poll-based (no interrupts). Reads 6-byte HID tablet reports (absolute coords).

use crate::{pmm, vmm, serial};
use spin::Mutex;

// ─── MMIO helpers ──────────────────────────────────────────────────────────────
unsafe fn r8 (b: *const u8, o: usize) -> u8  { b.add(o).read_volatile() }
unsafe fn r32(b: *const u8, o: usize) -> u32 { (b.add(o) as *const u32).read_volatile() }
unsafe fn w32(b: *mut   u8, o: usize, v: u32){ (b.add(o) as *mut   u32).write_volatile(v) }
unsafe fn w64(b: *mut   u8, o: usize, v: u64){ (b.add(o) as *mut   u64).write_volatile(v) }

// ─── Capability register offsets ───────────────────────────────────────────────
const CAP_HCSPARAMS1: usize = 0x04;
const CAP_DBOFF:      usize = 0x14;
const CAP_RTSOFF:     usize = 0x18;

// ─── Operational register offsets (cap_base + CAPLENGTH) ───────────────────────
const OP_USBCMD: usize = 0x00;
const OP_USBSTS: usize = 0x04;
const OP_DNCTRL: usize = 0x14;
const OP_CRCR:   usize = 0x18;
const OP_DCBAAP: usize = 0x30;
const OP_CONFIG: usize = 0x38;

const CMD_RUN:   u32 = 1 << 0;
const CMD_HCRST: u32 = 1 << 1;
const STS_HCH:   u32 = 1 << 0;
const STS_CNR:   u32 = 1 << 11;

// Port registers (op_base + 0x400 + port_idx * 0x10)
const PORT_CCS:  u32 = 1 << 0;
const PORT_PED:  u32 = 1 << 1;
const PORT_PR:   u32 = 1 << 4;
const PORT_PP:   u32 = 1 << 9;
const PORT_PRC:  u32 = 1 << 21;
// Bits 17-23 are write-1-to-clear status change bits
const PORT_W1C:  u32 = 0x00FE_0000;

// ─── Runtime interrupter 0 offsets (rt_base + 0x20) ───────────────────────────
const IR0_ERSTSZ: usize = 0x08;
const IR0_ERSTBA: usize = 0x10;
const IR0_ERDP:   usize = 0x18;

// ─── TRB types ─────────────────────────────────────────────────────────────────
const TRB_NORMAL:      u32 = 1;
const TRB_SETUP:       u32 = 2;
const TRB_STATUS:      u32 = 4;
const TRB_LINK:        u32 = 6;
const TRB_CMD_EN_SLOT: u32 = 9;
const TRB_CMD_ADDR:    u32 = 11;
const TRB_CMD_CFG_EP:  u32 = 12;
const TRB_EV_XFER:     u32 = 32;
const TRB_EV_CMD:      u32 = 33;

const CC_SUCCESS: u32 = 1;
const CC_SHORT:   u32 = 13;

const RING_N: usize = 64;

fn dma() -> (u64, *mut u8) {
    let p = pmm::alloc_page().expect("xhci: DMA OOM");
    let v = vmm::phys_to_virt(p);
    unsafe { core::ptr::write_bytes(v, 0, 4096); }
    (p, v)
}

unsafe fn trb_w(base: *mut u8, idx: usize, w: [u32; 4]) {
    let p = base.add(idx * 16) as *mut u32;
    for i in 0..4 { p.add(i).write_volatile(w[i]); }
}
unsafe fn trb_r(base: *const u8, idx: usize) -> [u32; 4] {
    let p = base.add(idx * 16) as *const u32;
    [p.read_volatile(), p.add(1).read_volatile(),
     p.add(2).read_volatile(), p.add(3).read_volatile()]
}

pub struct Xhci {
    _cap: *mut u8, _op: *mut u8, rt: *mut u8, db: *mut u8,

    cmd_v: *mut u8, cmd_p: u64, cmd_i: usize, cmd_c: u8,
    evt_v: *mut u8, evt_p: u64, evt_i: usize, evt_c: u8,

    _erst_v: *mut u8,
    _dcbaa_v: *mut u8,
    _dev_ctx_v: *mut u8,
    _in_ctx_v: *mut u8,

    ep0_v: *mut u8, ep0_p: u64, ep0_i: usize, ep0_c: u8,
    hid_v: *mut u8, hid_p: u64, hid_i: usize, hid_c: u8,
    hid_buf_v: *mut u8, hid_buf_p: u64,

    slot: u8,
}

unsafe impl Send for Xhci {}

impl Xhci {
    unsafe fn ring_cmd(&self) { (self.db as *mut u32).write_volatile(0); }
    unsafe fn ring_ep(&self, dci: u8) {
        (self.db.add(self.slot as usize * 4) as *mut u32).write_volatile(dci as u32);
    }

    unsafe fn push_cmd(&mut self, mut w: [u32; 4]) {
        w[3] = (w[3] & !1) | self.cmd_c as u32;
        trb_w(self.cmd_v, self.cmd_i, w);
        self.cmd_i += 1;
        if self.cmd_i >= RING_N - 1 {
            let tc = if self.cmd_c == 1 { 1u32 << 1 } else { 0 };
            trb_w(self.cmd_v, self.cmd_i, [
                self.cmd_p as u32, (self.cmd_p >> 32) as u32, 0,
                TRB_LINK << 10 | tc | self.cmd_c as u32,
            ]);
            self.cmd_i = 0;
            self.cmd_c ^= 1;
        }
    }

    unsafe fn push_ep0(&mut self, mut w: [u32; 4]) {
        w[3] = (w[3] & !1) | self.ep0_c as u32;
        trb_w(self.ep0_v, self.ep0_i, w);
        self.ep0_i += 1;
        if self.ep0_i >= RING_N - 1 {
            let tc = if self.ep0_c == 1 { 1u32 << 1 } else { 0 };
            trb_w(self.ep0_v, self.ep0_i, [
                self.ep0_p as u32, (self.ep0_p >> 32) as u32, 0,
                TRB_LINK << 10 | tc | self.ep0_c as u32,
            ]);
            self.ep0_i = 0;
            self.ep0_c ^= 1;
        }
    }

    unsafe fn dequeue(&mut self) -> Option<[u32; 4]> {
        let trb = trb_r(self.evt_v, self.evt_i);
        if (trb[3] & 1) != self.evt_c as u32 { return None; }
        let erdp = self.evt_p + self.evt_i as u64 * 16;
        w64(self.rt, 0x20 + IR0_ERDP, erdp | 8);
        self.evt_i += 1;
        if self.evt_i >= RING_N { self.evt_i = 0; self.evt_c ^= 1; }
        Some(trb)
    }

    // Wait for Command Completion Event; return (completion_code, slot_id)
    unsafe fn wait_cmd(&mut self) -> (u32, u8) {
        for _ in 0..8_000_000u32 {
            if let Some(t) = self.dequeue() {
                let ty = (t[3] >> 10) & 0x3F;
                if ty == TRB_EV_CMD {
                    let cc   = (t[2] >> 24) & 0xFF;
                    let slot = (t[3] >> 24) as u8;
                    return (cc, slot);
                }
                // Eat port-status-change events silently
                continue;
            }
            core::hint::spin_loop();
        }
        serial::print("xhci: wait_cmd timeout!\n");
        (0xFF, 0)
    }

    // Wait for one Transfer Event
    unsafe fn wait_xfer(&mut self) {
        for _ in 0..8_000_000u32 {
            if let Some(t) = self.dequeue() {
                if (t[3] >> 10) & 0x3F == TRB_EV_XFER { return; }
                continue;
            }
            core::hint::spin_loop();
        }
        serial::print("xhci: wait_xfer timeout!\n");
    }

    // No-data OUT control transfer (e.g. SET_CONFIGURATION)
    unsafe fn ctrl_nodata(&mut self, bm: u8, req: u8, val: u16, idx: u16) {
        let w0 = bm as u32 | ((req as u32) << 8) | ((val as u32) << 16);
        let w1 = idx as u32;
        // Setup TRB: IDT=bit6, IOC=0, TRT=0 (no data stage)
        self.push_ep0([w0, w1, 8, TRB_SETUP << 10 | 1 << 6]);
        // Status TRB: DIR=IN (bit16), IOC=bit5
        self.push_ep0([0, 0, 0, TRB_STATUS << 10 | 1 << 16 | 1 << 5]);
        self.ring_ep(1);
        self.wait_xfer();
    }

    unsafe fn queue_hid(&mut self) {
        let c = self.hid_c as u32;
        trb_w(self.hid_v, self.hid_i, [
            self.hid_buf_p as u32, (self.hid_buf_p >> 32) as u32,
            8, TRB_NORMAL << 10 | 1 << 5 | c,
        ]);
        self.hid_i += 1;
        if self.hid_i >= RING_N - 1 {
            // TC=1 (Toggle Cycle) ALWAYS so the XHC toggles PCS on every wrap.
            // cycle bit = c (current cycle, so XHC processes this link TRB now).
            trb_w(self.hid_v, self.hid_i, [
                self.hid_p as u32, (self.hid_p >> 32) as u32,
                0, TRB_LINK << 10 | (1 << 1) | c,
            ]);
            self.hid_i = 0;
            self.hid_c ^= 1;
        }
        self.ring_ep(3); // EP1 IN = DCI 3
    }

    pub fn poll(&mut self, fb_w: u32, fb_h: u32) {
        unsafe {
            while let Some(t) = self.dequeue() {
                let ty = (t[3] >> 10) & 0x3F;
                let cc = (t[2] >> 24) & 0xFF;
                if ty == TRB_EV_XFER && (cc == CC_SUCCESS || cc == CC_SHORT) {
                    let buf = core::slice::from_raw_parts(self.hid_buf_v, 8);
                    let buttons = buf[0] & 0x07;
                    let abs_x   = u16::from_le_bytes([buf[1], buf[2]]) as u32;
                    let abs_y   = u16::from_le_bytes([buf[3], buf[4]]) as u32;
                    // Ignore (0,0) with no buttons — tablet sends this as initial report
                    // before the host cursor enters the QEMU window.
                    if abs_x != 0 || abs_y != 0 || buttons != 0 {
                        let sx = (abs_x.saturating_mul(fb_w)) / 32768;
                        let sy = (abs_y.saturating_mul(fb_h)) / 32768;
                        let mut m = crate::mouse::MOUSE.lock();
                        m.x = sx as i32;
                        m.y = sy as i32;
                        m.buttons = buttons;
                    }
                    core::ptr::write_bytes(self.hid_buf_v, 0, 8);
                    self.queue_hid();
                }
            }
        }
    }
}

pub static XHCI: Mutex<Option<Xhci>> = Mutex::new(None);

pub fn poll_mouse(fb_w: u32, fb_h: u32) {
    if let Some(x) = XHCI.lock().as_mut() { x.poll(fb_w, fb_h); }
}

/// Returns true if XHCI is initialized and ready.
pub fn is_ready() -> bool { XHCI.lock().is_some() }

pub fn init(devices: &[crate::pci::PciDevice]) {
    let d = match devices.iter().find(|d|
        d.class == 0x0C && d.subclass == 0x03 && d.prog_if == 0x30)
    {
        Some(d) => d,
        None => { serial::print("xhci: no XHCI controller\n"); return; }
    };
    serial::print("xhci: found controller\n");
    serial::print_hex("  vendor:dev=", ((d.vendor_id as u64) << 16) | d.device_id as u64);

    // Enable MMIO + bus master
    let pci_cmd = crate::pci::config_read16(d.bus, d.dev, d.func, 0x04);
    crate::pci::config_write32(d.bus, d.dev, d.func, 0x04, (pci_cmd | 0x06) as u32);

    // BAR0 (64-bit MMIO for qemu-xhci)
    let bar0 = d.bar(0);
    let bar1 = d.bar(1);
    let bar_phys = if (bar0 & 0x06) == 0x04 {
        (bar0 as u64 & !0xF) | ((bar1 as u64) << 32)
    } else {
        bar0 as u64 & !0xF
    };
    serial::print_hex("xhci: BAR=", bar_phys);
    if bar_phys == 0 { serial::print("xhci: BAR is 0, aborting\n"); return; }

    let cap = crate::paging::map_mmio(bar_phys, 65536);

    unsafe {
        let cap_len  = r8(cap as *const u8, 0) as usize;
        let op       = cap.add(cap_len);
        let db_off   = (r32(cap as *const u8, CAP_DBOFF)  & !3)  as usize;
        let rt_off   = (r32(cap as *const u8, CAP_RTSOFF) & !31) as usize;
        let db       = cap.add(db_off);
        let rt       = cap.add(rt_off);

        let hcsp1    = r32(cap as *const u8, CAP_HCSPARAMS1);
        let max_slots= (hcsp1 & 0xFF) as usize;
        let max_ports= (hcsp1 >> 24) as usize;
        serial::print_hex("xhci: cap_len=", cap_len as u64);
        serial::print_hex("xhci: max_ports=", max_ports as u64);

        // Wait until controller is ready
        for _ in 0..4_000_000u32 {
            if r32(op as *const u8, OP_USBSTS) & STS_CNR == 0 { break; }
            core::hint::spin_loop();
        }
        let sts = r32(op as *const u8, OP_USBSTS);
        serial::print_hex("xhci: USBSTS before reset=", sts as u64);

        // Stop if running
        if r32(op as *const u8, OP_USBCMD) & CMD_RUN != 0 {
            w32(op, OP_USBCMD, r32(op as *const u8, OP_USBCMD) & !CMD_RUN);
            for _ in 0..4_000_000u32 {
                if r32(op as *const u8, OP_USBSTS) & STS_HCH != 0 { break; }
                core::hint::spin_loop();
            }
        }

        // HC reset
        w32(op, OP_USBCMD, CMD_HCRST);
        for _ in 0..4_000_000u32 {
            if r32(op as *const u8, OP_USBCMD) & CMD_HCRST == 0 { break; }
            core::hint::spin_loop();
        }
        for _ in 0..4_000_000u32 {
            if r32(op as *const u8, OP_USBSTS) & STS_CNR == 0 { break; }
            core::hint::spin_loop();
        }
        serial::print("xhci: reset done\n");

        // Allocate DMA pages (all zeroed by dma())
        let (cmd_p, cmd_v)         = dma();
        let (evt_p, evt_v)         = dma();
        let (erst_p, erst_v)       = dma();
        let (dcbaa_p, dcbaa_v)     = dma();
        let (dev_ctx_p, dev_ctx_v) = dma();
        let (in_ctx_p, in_ctx_v)   = dma();
        let (ep0_p, ep0_v)         = dma();
        let (hid_p, hid_v)         = dma();
        let (hid_buf_p, hid_buf_v) = dma();

        // Place link TRBs at end of each ring
        macro_rules! link_trb {
            ($v:expr, $p:expr) => {
                trb_w($v, RING_N - 1, [
                    $p as u32, ($p >> 32) as u32,
                    0, TRB_LINK << 10 | 1 << 1 | 1, // TC=1, cycle=1
                ]);
            }
        }
        link_trb!(cmd_v, cmd_p);
        link_trb!(ep0_v, ep0_p);
        link_trb!(hid_v, hid_p);

        // ERST: 1 segment pointing to event ring
        (erst_v as *mut u64).write_volatile(evt_p);
        (erst_v.add(8) as *mut u32).write_volatile(RING_N as u32);

        // Interrupter 0
        let ir0 = rt.add(0x20);
        w32(ir0, IR0_ERSTSZ, 1);
        w64(ir0, IR0_ERSTBA, erst_p);
        w64(ir0, IR0_ERDP, evt_p);

        // Operational setup
        w64(op, OP_DCBAAP, dcbaa_p);
        w32(op, OP_CONFIG, max_slots.min(255) as u32);
        w32(op, OP_DNCTRL, 0xFFFF);
        w64(op, OP_CRCR, cmd_p | 1); // CCS=1

        // Start HC
        w32(op, OP_USBCMD, CMD_RUN | 1 << 2 | 1 << 3);
        for _ in 0..4_000_000u32 {
            if r32(op as *const u8, OP_USBSTS) & STS_HCH == 0 { break; }
            core::hint::spin_loop();
        }
        serial::print_hex("xhci: USBSTS after start=", r32(op as *const u8, OP_USBSTS) as u64);
        serial::print("xhci: HC running\n");

        // Small delay for USB devices to appear on ports
        for _ in 0..1_000_000u32 { core::hint::spin_loop(); }

        // Scan ports — print PORTSC for each, find first connected
        let mut connected_port = 0u8;
        let mut port_speed = 3u8; // default: High Speed
        for pi in 0..max_ports {
            let pb = op.add(0x400 + pi * 0x10);
            let sc = r32(pb as *const u8, 0);
            serial::print_hex("xhci: port PORTSC=", sc as u64);

            // Power on if needed
            if sc & PORT_PP == 0 {
                w32(pb, 0, (sc & !PORT_W1C) | PORT_PP);
                for _ in 0..500_000u32 { core::hint::spin_loop(); }
            }

            let sc = r32(pb as *const u8, 0);
            if sc & PORT_CCS != 0 && connected_port == 0 {
                serial::print_hex("xhci: device on port (1-based)=", (pi + 1) as u64);
                connected_port = (pi + 1) as u8;

                // Reset port: clear W1C bits, set PR
                w32(pb, 0, (sc & !PORT_W1C) | PORT_PR);
                for _ in 0..4_000_000u32 {
                    if r32(pb as *const u8, 0) & PORT_PR == 0 { break; }
                    core::hint::spin_loop();
                }

                // Read port speed AFTER reset (bits[13:10])
                let sc2 = r32(pb as *const u8, 0);
                port_speed = ((sc2 >> 10) & 0xF) as u8;
                if port_speed == 0 { port_speed = 3; } // default to HS if unknown
                serial::print_hex("xhci: port speed=", port_speed as u64);
                serial::print_hex("xhci: PORTSC after reset=", sc2 as u64);

                // Clear PRC change bit
                w32(pb, 0, (sc2 & !PORT_W1C) | PORT_PRC);
                serial::print("xhci: port reset done\n");
            }
        }

        if connected_port == 0 {
            serial::print("xhci: no device found on any port!\n");
            return;
        }

        // Small delay before sending commands
        for _ in 0..200_000u32 { core::hint::spin_loop(); }

        let mut x = Xhci {
            _cap: cap, _op: op, rt, db,
            cmd_v, cmd_p, cmd_i: 0, cmd_c: 1,
            evt_v, evt_p, evt_i: 0, evt_c: 1,
            _erst_v: erst_v,
            _dcbaa_v: dcbaa_v,
            _dev_ctx_v: dev_ctx_v,
            _in_ctx_v: in_ctx_v,
            ep0_v, ep0_p, ep0_i: 0, ep0_c: 1,
            hid_v, hid_p, hid_i: 0, hid_c: 1,
            hid_buf_v, hid_buf_p,
            slot: 0,
        };

        // ── Enable Slot ────────────────────────────────────────────────────────
        serial::print("xhci: sending Enable Slot...\n");
        x.push_cmd([0, 0, 0, TRB_CMD_EN_SLOT << 10]);
        x.ring_cmd();
        let (cc, slot) = x.wait_cmd();
        serial::print_hex("xhci: Enable Slot CC=", cc as u64);
        if cc != CC_SUCCESS { return; }
        x.slot = slot;
        serial::print_hex("xhci: slot_id=", slot as u64);

        // DCBAAP[slot] → device context
        (dcbaa_v as *mut u64).add(slot as usize).write_volatile(dev_ctx_p);

        // ── Address Device ─────────────────────────────────────────────────────
        // Input context layout (each section = 0x20 bytes):
        //   0x00  Input Control Context
        //   0x20  Slot Context
        //   0x40  EP0 Context (DCI 1)
        //   0x60  EP1 OUT Context (DCI 2) — unused
        //   0x80  EP1 IN Context (DCI 3)  — added by Configure Endpoint
        let ic = in_ctx_v;

        // Input Control: Add A0 (slot) + A1 (EP0)
        (ic.add(0x04) as *mut u32).write_volatile(0x3);

        // Slot Context DW0: Context Entries=1, Speed from port, Route=0
        (ic.add(0x20) as *mut u32).write_volatile(1 << 27 | (port_speed as u32) << 20);
        // Slot Context DW1: Root Hub Port Number (1-based)
        (ic.add(0x24) as *mut u32).write_volatile((connected_port as u32) << 16);

        // EP0 Context DW1: Cerr=3, EP Type=4 (Control Bidir), Max Packet Size=64
        (ic.add(0x44) as *mut u32).write_volatile(3 << 1 | 4 << 3 | 64 << 16);
        // EP0 DW2+3: TR Dequeue Pointer + DCS=1
        (ic.add(0x48) as *mut u64).write_volatile(ep0_p | 1);
        // EP0 DW4: Average TRB Length=8
        (ic.add(0x50) as *mut u32).write_volatile(8);

        serial::print("xhci: sending Address Device...\n");
        x.push_cmd([
            in_ctx_p as u32 & !0xF,
            (in_ctx_p >> 32) as u32,
            0,
            TRB_CMD_ADDR << 10 | (slot as u32) << 24,
        ]);
        x.ring_cmd();
        let (cc, _) = x.wait_cmd();
        serial::print_hex("xhci: Address Device CC=", cc as u64);
        if cc != CC_SUCCESS { return; }
        serial::print("xhci: device addressed\n");

        // ── SET_CONFIGURATION(1) ───────────────────────────────────────────────
        serial::print("xhci: sending SET_CONFIGURATION...\n");
        x.ctrl_nodata(0x00, 0x09, 1, 0);
        serial::print("xhci: SET_CONFIGURATION done\n");

        // ── Configure Endpoint — add EP1 IN ────────────────────────────────────
        // Update input context IN PLACE (don't zero — keep slot DW1 port number).
        // Change Add flags to A0 (update slot) + A3 (add EP1 IN DCI 3).
        (ic.add(0x00) as *mut u32).write_volatile(0);       // Drop flags: none
        (ic.add(0x04) as *mut u32).write_volatile(1 | 1 << 3); // Add A0+A3

        // Slot DW0: Context Entries=3 (covers DCI 3), keep speed from before
        (ic.add(0x20) as *mut u32).write_volatile(3 << 27 | (port_speed as u32) << 20);
        // Slot DW1: port number unchanged (already set)

        // EP1 IN Context at offset 0x80 (DCI 3)
        // DW0: Interval=3 (2^3 μframes = 1ms for HS) at bits[23:16]
        (ic.add(0x80) as *mut u32).write_volatile(3 << 16);
        // DW1: Cerr=3, EP Type=7 (Interrupt IN), Max Packet Size=8
        (ic.add(0x84) as *mut u32).write_volatile(3 << 1 | 7 << 3 | 8 << 16);
        // DW2+3: TR Dequeue Pointer + DCS=1
        (ic.add(0x88) as *mut u64).write_volatile(hid_p | 1);
        // DW4: Average TRB Length=8
        (ic.add(0x90) as *mut u32).write_volatile(8);

        serial::print("xhci: sending Configure Endpoint...\n");
        x.push_cmd([
            in_ctx_p as u32 & !0xF,
            (in_ctx_p >> 32) as u32,
            0,
            TRB_CMD_CFG_EP << 10 | (slot as u32) << 24,
        ]);
        x.ring_cmd();
        let (cc, _) = x.wait_cmd();
        serial::print_hex("xhci: Configure EP CC=", cc as u64);
        if cc != CC_SUCCESS { return; }
        serial::print("xhci: EP1 IN ready\n");

        // Queue first HID transfer TRB
        x.queue_hid();
        serial::print("xhci: mouse ready!\n");
        *XHCI.lock() = Some(x);
    }
}
