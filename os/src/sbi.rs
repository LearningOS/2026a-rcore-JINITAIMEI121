//! SBI call wrappers

use core::arch::asm;

const SBI_SET_TIMER: usize = 0;
const SBI_CONSOLE_PUTCHAR: usize = 1;
const SBI_SYSTEM_RESET: usize = 0x53525354;
/// Supervisor software interrupt extension (EID `"sSI"`, FID 0 = Send IPI).
///
/// RustSBI's machine environment raises a *supervisor software interrupt* for
/// this call and then returns to S-mode with `mstatus.MIE` restored, so the
/// interrupt is taken as soon as we re-enable interrupts / return to user mode.
///
/// We use it as the only way to preempt a user program that spins in user mode
/// without ever entering the kernel (see `trap::run_user_and_preempt()`),
/// because QEMU's virt machine has no SSTC: `stimecmp` does not exist, so an S
/// timer can only be armed through the legacy M timer, which RustSBI keeps
/// undelegated in `mideleg` and therefore never surfaces as a *supervisor
/// timer* interrupt.
const SBI_SSI_SEND_IPI: usize = 0x7353_4900; // "sSI" == 's','S','I',0

/// general sbi call
#[inline(always)]
fn sbi_call(which: usize, arg0: usize, arg1: usize, arg2: usize) -> usize {
    let mut ret;
    unsafe {
        asm!(
            "ecall",
            inlateout("x10") arg0 => ret,
            in("x11") arg1,
            in("x12") arg2,
            in("x16") 0,
            in("x17") which,
        );
    }
    ret
}

/// Raise a supervisor software interrupt on the current hart via the firmware.
///
/// This is the only preemption source available to S-mode here: QEMU's virt
/// machine has no SSTC, so `stimecmp` cannot be written from the kernel at all,
/// and RustSBI keeps SSIP undelegated in `mideleg`, so an M timer tick never
/// becomes a *supervisor* timer interrupt (see the baseline behaviour where
/// tasks are only ever switched by `sys_yield`). We ask the firmware for an
/// S-mode software interrupt instead, which SBI guarantees to deliver to
/// S-mode as soon as we return there.
pub fn raise_supervisor_software_interrupt() {
    sbi_call(SBI_SSI_SEND_IPI, 0, 0, 0);
}

/// use sbi call to set timer
pub fn set_timer(timer: usize) {
    sbi_call(SBI_SET_TIMER, timer, 0, 0);
}

/// use sbi call to putchar in console (qemu uart handler)
pub fn console_putchar(c: usize) {
    sbi_call(SBI_CONSOLE_PUTCHAR, c, 0, 0);
}

/// use sbi call to shutdown the kernel
pub fn shutdown() -> ! {
    // SBI System Reset: shutdown (type 0), no failure reason (reason 0).
    unsafe {
        asm!(
            "ecall",
            in("a7") SBI_SYSTEM_RESET,
            in("a6") 0,
            inlateout("a0") 0usize => _,
            inlateout("a1") 0usize => _,
        );
    }
    panic!("It should shutdown!");
}
