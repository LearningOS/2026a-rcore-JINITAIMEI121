//! Trap handling functionality
//!
//! For rCore, we have a single trap entry point, namely `__alltraps`. At
//! initialization in [`init()`], we set the `stvec` CSR to point to it.
//!
//! All traps go through `__alltraps`, which is defined in `trap.S`. The
//! assembly language code does just enough work restore the kernel space
//! context, ensuring that Rust code safely runs, and transfers control to
//! [`trap_handler()`].
//!
//! It then calls different functionality based on what exactly the exception
//! was. For example, timer interrupts trigger task preemption, and syscalls go
//! to [`syscall()`].

mod context;

pub use context::TrapContext;

use crate::sbi::raise_supervisor_software_interrupt;
use crate::syscall::{is_supported, syscall};
use crate::task::{exit_current_and_run_next, record_syscall, suspend_current_and_run_next};
use crate::timer::set_next_trigger;
use core::arch::global_asm;
use riscv::register::{
    mtvec::TrapMode,
    scause::{self, Exception, Interrupt, Trap},
    sie, stval, stvec,
};

global_asm!(include_str!("trap.S"));

/// `sie.SSIE` bit position. The `riscv` crate used by this tutorial exposes
/// `set_ssoft`/`clear_ssoft` for `sie` but no getter, and we have to read the
/// bit back in [`return_to_user()`] to decide whether firmware-assisted
/// preemption is needed at all.
const SSIE_BIT: usize = 1 << 1;

/// `sip.SSIP` bit position (the pending bit of the supervisor software
/// interrupt). `sip` has no setter in this crate either.
const SSIP_BIT: usize = 1 << 1;

/// Read a raw CSR without the missing `riscv` crate wrappers.
fn read_raw_sie() -> usize {
    let bits: usize;
    unsafe {
        core::arch::asm!("csrr {}, sie", out(reg) bits);
    }
    bits
}

fn read_raw_sip() -> usize {
    let bits: usize;
    unsafe {
        core::arch::asm!("csrr {}, sip", out(reg) bits);
    }
    bits
}

fn write_raw_sip(bits: usize) {
    unsafe {
        core::arch::asm!("csrw sip, {}", in(reg) bits);
    }
}

/// Initialize trap handling
pub fn init() {
    extern "C" {
        fn __alltraps();
    }
    unsafe {
        stvec::write(__alltraps as usize, TrapMode::Direct);
    }
}

/// enable timer interrupt in supervisor mode
pub fn enable_timer_interrupt() {
    unsafe {
        sie::set_stimer();
    }
}

/// Is the supervisor software interrupt usable by S-mode on this platform?
///
/// It is only ours if both the enable bit (`sie.SSIE`) and the request bit
/// (`sip.SSIP`) are writable, which requires the firmware to have delegated
/// them in `mideleg`. RustSBI does not, so on this machine the answer is no and
/// [`return_to_user()`] takes the firmware-assisted path instead.
fn supervisor_soft_irq_delegated() -> bool {
    unsafe {
        sie::set_ssoft();
        let ssie_usable = read_raw_sie() & SSIE_BIT != 0;
        sie::clear_ssoft();
        write_raw_sip(read_raw_sip() | SSIP_BIT);
        let ssip_writable = read_raw_sip() & SSIP_BIT != 0;
        // Leave the request bit clear whichever way this probe went.
        write_raw_sip(read_raw_sip() & !SSIP_BIT);
        ssie_usable && ssip_writable
    }
}

/// Return to user mode, making sure a task cannot hold the CPU forever without
/// ever coming back to the kernel.
///
/// Without the Sv39 page tables of chapter 4 the kernel has no way to write
/// `stimecmp`, and QEMU's virt machine does not implement SSTC, so the machine
/// timer stays owned by the firmware and never surfaces as a *supervisor timer*
/// interrupt. A user program that neither calls a syscall nor faults would thus
/// run forever (`ch3_trace` does exactly that while its syscalls are still
/// unimplemented, and `ch2b_bad_register` would too if it looped instead of
/// exiting). We therefore ask the firmware for a supervisor software interrupt
/// right before leaving the kernel: RustSBI sets `mip.SSIP` and restores
/// `mstatus.MIE` on `mret`, so the interrupt is taken as soon as the task
/// resumes in U-mode and lands back in [`trap_handler()`]. That gives every
/// task a time slice even though the kernel has no working comparator.
///
/// When the platform does delegate the supervisor software interrupt, the plain
/// `sret` path is kept and that interrupt stays the sole preemption source.
fn return_to_user(cx: &mut TrapContext) -> &mut TrapContext {
    if supervisor_soft_irq_delegated() {
        // A real S-mode software interrupt is available: keep it masked while
        // we are here and let the periodic tick preempt the task.
        unsafe {
            sie::clear_ssoft();
        }
        return cx;
    }
    // Enter user mode with supervisor software interrupts masked so that the
    // request we are about to make cannot fire while we are still in the
    // kernel.
    unsafe {
        sie::clear_ssoft();
    }
    raise_supervisor_software_interrupt();
    // Jump straight into the assembly return path with `ra` set to `__restore`:
    // `__switch` saved the previous execution's context on *this* kernel stack,
    // so we must not unwind through it. `__restore` leaves with `sret`, hence
    // the diverging return type.
    extern "C" {
        fn __restore();
    }
    unsafe {
        core::arch::asm!(
            "jr {restore}",
            restore = in(reg) __restore as usize,
            in("a0") cx as *mut TrapContext,
            options(noreturn),
        );
    }
}

/// Clear a pending supervisor software interrupt.
///
/// Only when the request bit belongs to S-mode may we clear it; otherwise the
/// firmware owns it and clears it as part of delivering the interrupt.
fn clear_supervisor_soft_pending() {
    let bits = read_raw_sip();
    if bits & SSIP_BIT != 0 {
        write_raw_sip(bits & !SSIP_BIT);
    }
}

/// trap handler
#[no_mangle]
pub fn trap_handler(cx: &mut TrapContext) -> &mut TrapContext {
    let scause = scause::read(); // get trap cause
    let stval = stval::read(); // get extra value
                               // trace!("into {:?}", scause.cause());
    match scause.cause() {
        Trap::Exception(Exception::UserEnvCall) => {
            let syscall_id = cx.x[17];
            // Record the invocation *before* dispatching it: `ch3_trace` asks
            // `count_syscall(SYSCALL_TRACE)` and expects the asking call itself
            // to be part of the answer. An id this kernel cannot dispatch is not
            // counted, because it will never reach an application again.
            if is_supported(syscall_id) {
                record_syscall(syscall_id);
            }
            // jump to next instruction anyway
            cx.sepc += 4;
            // get system call return value
            cx.x[10] = syscall(syscall_id, [cx.x[10], cx.x[11], cx.x[12]]) as usize;
        }
        Trap::Exception(Exception::StoreFault) | Trap::Exception(Exception::StorePageFault) => {
            println!("[kernel] PageFault in application, bad addr = {:#x}, bad instruction = {:#x}, kernel killed it.", stval, cx.sepc);
            exit_current_and_run_next();
        }
        Trap::Exception(Exception::IllegalInstruction) => {
            // The firmware emulates a few otherwise-forbidden CSRs (`time`,
            // `timeh`) and forwards everything else to us, so an illegal
            // instruction can also be a supervisor CSR accessed from U-mode.
            // Such a register is not part of an application's state at all: it
            // must never reach the program, neither as a value nor as a fault
            // it can react to, so we drop the task right here.
            println!("[kernel] IllegalInstruction in application, kernel killed it.");
            exit_current_and_run_next();
        }
        Trap::Interrupt(Interrupt::SupervisorTimer) => {
            set_next_trigger();
            suspend_current_and_run_next();
        }
        Trap::Interrupt(Interrupt::SupervisorSoft) => {
            // Clear the request first so we can never spin on it. Note that we
            // deliberately do *not* call `set_next_trigger()` here: the machine
            // timer is not ours to re-arm on this path, and the next time slice
            // request is made once per scheduling decision in
            // [`return_to_user()`].
            clear_supervisor_soft_pending();
            suspend_current_and_run_next();
        }
        _ => {
            panic!(
                "Unsupported trap {:?}, stval = {:#x}!",
                scause.cause(),
                stval
            );
        }
    }
    return_to_user(cx)
}
