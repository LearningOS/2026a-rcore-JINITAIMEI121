//! Implementation of syscalls
//!
//! The single entry point to all system calls, [`syscall()`], is called
//! whenever userspace wishes to perform a system call using the `ecall`
//! instruction. In this case, the processor raises an 'Environment call from
//! U-mode' exception, which is handled as one of the cases in
//! [`crate::trap::trap_handler`].
//!
//! For clarity, each single syscall is implemented as its own function, named
//! `sys_` then the name of the syscall. You should also implement syscalls this
//! way.

/// write syscall
pub const SYSCALL_WRITE: usize = 64;
/// exit syscall
pub const SYSCALL_EXIT: usize = 93;
/// yield syscall
pub const SYSCALL_YIELD: usize = 124;
/// gettime syscall
pub const SYSCALL_GET_TIME: usize = 169;
/// trace syscall
pub const SYSCALL_TRACE: usize = 410;

mod fs;
mod process;

use fs::*;
use process::*;

/// Validate a user buffer before the kernel touches it: is
/// `[addr, addr + len)` entirely inside the region handed to the application
/// that is currently running?
///
/// Chapter 3 has no page tables: every app lives in a static slot of
/// `APP_SIZE_LIMIT` bytes starting at `APP_BASE_ADDRESS`, so this range check is
/// the only memory protection available. It lets the kernel refuse a bad user
/// pointer instead of dereferencing it and faulting itself.
pub fn user_addr_valid(addr: usize, len: usize) -> bool {
    if len == 0 {
        return false;
    }
    let limit = match addr.checked_add(len) {
        Some(limit) => limit,
        None => return false,
    };
    crate::task::current_user_ranges().map_or(false, |ranges| {
        ranges
            .iter()
            .any(|(start, end)| addr >= *start && limit <= *end)
    })
}

/// Does this kernel know how to dispatch `syscall_id`?
pub fn is_supported(syscall_id: usize) -> bool {
    matches!(
        syscall_id,
        SYSCALL_WRITE | SYSCALL_EXIT | SYSCALL_YIELD | SYSCALL_GET_TIME | SYSCALL_TRACE
    )
}

/// handle syscall exception with `syscall_id` and other arguments
pub fn syscall(syscall_id: usize, args: [usize; 3]) -> isize {
    match syscall_id {
        SYSCALL_WRITE => sys_write(args[0], args[1] as *const u8, args[2]),
        SYSCALL_EXIT => sys_exit(args[0] as i32),
        SYSCALL_YIELD => sys_yield(),
        SYSCALL_GET_TIME => sys_get_time(args[0] as *mut TimeVal, args[1]),
        SYSCALL_TRACE => sys_trace(args[0], args[1], args[2]),
        // An unknown id is the application's bug, not the kernel's: report it
        // and let the task die instead of panicking, which would take the whole
        // system (and every other task's output) down with it.
        _ => {
            println!(
                "[kernel] Unsupported syscall_id: {}, kernel killed the application.",
                syscall_id
            );
            -1
        }
    }
}
