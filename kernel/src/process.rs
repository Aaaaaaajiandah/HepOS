//! Minimal ring-3 process support.
//!
//! `run_test()` sets up a fresh user address space, copies a small embedded
//! program into it, and enters ring 3 via IRETQ.  The program calls:
//!   write(1, "Hello from ring 3!\n", 19)  → serial output
//!   exit(0)                                → longjmps back to kernel
//!
//! The APIC timer is masked for the duration so the scheduler does not
//! try to context-switch away mid-flight.

use crate::{apic, paging, pmm, vmm};

// ── User virtual address layout ───────────────────────────────────────────────

const USER_CODE_BASE:  u64 = 0x0040_0000;   // 4 MB — traditional ELF load address
const USER_STACK_PAGE: u64 = 0x7FFF_E000;   // one page mapped for the stack
const USER_STACK_TOP:  u64 = 0x7FFF_F000;   // RSP starts at page top

// ── Embedded user program (raw x86-64 machine code) ──────────────────────────
//
// Layout:
//   0:  mov rax, 1          (SYS_WRITE)
//   7:  mov rdi, 1          (fd = stdout)
//  14:  lea rsi, [rip+23]   → points to msg at offset 44
//  21:  mov rdx, 19         (len = 19 bytes)
//  28:  syscall
//  30:  mov rax, 60         (SYS_EXIT)
//  37:  xor rdi, rdi        (exit code 0)
//  40:  syscall
//  42:  jmp -2              (infinite loop — sys_exit longjmps back before this)
//  44:  "Hello from ring 3!\n"
static USER_PROGRAM: [u8; 63] = [
    // mov rax, 1
    0x48, 0xC7, 0xC0, 0x01, 0x00, 0x00, 0x00,
    // mov rdi, 1
    0x48, 0xC7, 0xC7, 0x01, 0x00, 0x00, 0x00,
    // lea rsi, [rip + 0x17]   (RIP at 21 after this instr; msg at 44; 44-21=23=0x17)
    0x48, 0x8D, 0x35, 0x17, 0x00, 0x00, 0x00,
    // mov rdx, 19
    0x48, 0xC7, 0xC2, 0x13, 0x00, 0x00, 0x00,
    // syscall
    0x0F, 0x05,
    // mov rax, 60
    0x48, 0xC7, 0xC0, 0x3C, 0x00, 0x00, 0x00,
    // xor rdi, rdi
    0x48, 0x31, 0xFF,
    // syscall
    0x0F, 0x05,
    // jmp -2  (fallback loop; sys_exit longjmps back before reaching this)
    0xEB, 0xFE,
    // "Hello from ring 3!\n"
    b'H', b'e', b'l', b'l', b'o', b' ',
    b'f', b'r', b'o', b'm', b' ',
    b'r', b'i', b'n', b'g', b' ',
    b'3', b'!', b'\n',
];

// ── Process state ─────────────────────────────────────────────────────────────

/// Set while a user process is executing; sys_exit checks this.
pub static mut USER_RUNNING: bool = false;

/// Saved kernel RSP from enter_ring3; do_exit restores it to return.
static mut KERNEL_RETURN_RSP: u64 = 0;

/// Exit code written by do_exit; read by run_test after return.
static mut EXIT_CODE: u64 = 0;

// ── CR3 helpers ───────────────────────────────────────────────────────────────

fn read_cr3() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) v, options(nostack, nomem)); }
    v & !0xFFF  // strip flags bits
}

unsafe fn write_cr3(phys: u64) {
    core::arch::asm!("mov cr3, {}", in(reg) phys, options(nostack, nomem));
}

// ── Page table setup ──────────────────────────────────────────────────────────

/// Allocate a fresh PML4 and copy the kernel high-half entries (256..511)
/// from the current PML4 so the kernel is reachable while the user PML4 is loaded.
fn create_user_pml4() -> u64 {
    let phys = pmm::alloc_page().expect("process: OOM for PML4");
    let virt = vmm::phys_to_virt(phys);
    unsafe {
        core::ptr::write_bytes(virt, 0, 4096);
        let cur  = vmm::phys_to_virt(read_cr3()) as *const u64;
        let new  = virt as *mut u64;
        for i in 256..512usize {
            new.add(i).write_volatile(cur.add(i).read_volatile());
        }
    }
    phys
}

// ── Entry / exit ──────────────────────────────────────────────────────────────

/// IRETQ into ring 3 at USER_CODE_BASE / USER_STACK_TOP.
///
/// On entry: push callee-saved regs + save RSP → KERNEL_RETURN_RSP.
/// "Returns" when do_exit() restores that RSP and executes ret.
#[unsafe(naked)]
unsafe extern "C" fn enter_ring3() {
    core::arch::naked_asm!(
        // Save all callee-saved registers so do_exit's longjmp restores them.
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // Record where the kernel stack is so do_exit can jump back here.
        "lea rax, [rip + {krsp}]",
        "mov [rax], rsp",
        // Build the IRETQ frame (CPU pops: RIP, CS, RFLAGS, RSP, SS).
        // Push in reverse: SS first, then RSP, RFLAGS, CS, RIP.
        "push 0x1b",            // SS  = USER_DS | RPL3
        "push 0x7ffff000",      // RSP = USER_STACK_TOP
        "push 0x202",           // RFLAGS: IF=1, bit-1 always set
        "push 0x23",            // CS  = USER_CS | RPL3
        "push 0x400000",        // RIP = USER_CODE_BASE
        "iretq",
        // ── do_exit longjmps back to here ────────────────────────────────────
        // (restores RSP to saved value above, then executes the pops + ret below)
        krsp = sym KERNEL_RETURN_RSP,
    );
}

/// Called by sys_exit.  Restores the kernel stack frame left by enter_ring3
/// and "returns" from it as if enter_ring3 had returned normally.
///
/// SAFETY: must only be called while USER_RUNNING == true and from within
/// the SYSCALL handling path (on the syscall kernel stack, not task_blink's stack).
pub unsafe fn do_exit(code: u64) -> ! {
    EXIT_CODE = code;
    USER_RUNNING = false;
    core::arch::asm!(
        "mov rsp, [{rsp}]",  // jump to kernel_return_rsp
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",               // returns from enter_ring3() → run_test() continues
        rsp = sym KERNEL_RETURN_RSP,
        options(noreturn),
    );
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Set up a user address space, enter ring 3, and wait for the process to exit.
/// Returns the process exit code.
pub fn run_test() -> u64 {
    // 1. Build the user page tables.
    let pml4 = create_user_pml4();

    // Code page: readable + user (exec is implicit — we don't set NX)
    let code_phys = pmm::alloc_page().expect("process: OOM for code page");
    let code_virt = vmm::phys_to_virt(code_phys);
    unsafe {
        core::ptr::write_bytes(code_virt, 0, 4096);
        core::ptr::copy_nonoverlapping(
            USER_PROGRAM.as_ptr(),
            code_virt,
            USER_PROGRAM.len(),
        );
    }
    paging::map_page_into(pml4, USER_CODE_BASE, code_phys, paging::USER | paging::WRITE);

    // Stack page: read/write + user
    let stack_phys = pmm::alloc_page().expect("process: OOM for stack page");
    unsafe { core::ptr::write_bytes(vmm::phys_to_virt(stack_phys), 0, 4096); }
    paging::map_page_into(pml4, USER_STACK_PAGE, stack_phys, paging::USER | paging::WRITE);

    // 2. Mask the timer so the scheduler doesn't preempt during ring-3 execution.
    apic::mask_timer();

    // 3. Switch to the user page tables and enter ring 3.
    let orig_cr3 = read_cr3();
    unsafe {
        write_cr3(pml4);
        USER_RUNNING = true;
        enter_ring3(); // "returns" when do_exit() is called from sys_exit
    }

    // 4. Restore kernel page tables and re-enable scheduling.
    unsafe { write_cr3(orig_cr3); }
    apic::unmask_timer();

    // 5. Free user pages.
    pmm::free_page(code_phys);
    pmm::free_page(stack_phys);
    // Note: intermediate page-table pages (PT, PD, PDP, PML4) are leaked for now.
    // Full cleanup would walk the user-space portion of pml4 and free every page.
    pmm::free_page(pml4);

    unsafe { EXIT_CODE }
}
