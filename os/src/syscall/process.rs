//! Process management syscalls
use super::user_addr_valid;
use crate::{
    task::{current_syscall_count, exit_current_and_run_next, suspend_current_and_run_next},
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("[kernel] Application exited with code {}", exit_code);
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// get time with second and microsecond
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    if !user_addr_valid(ts as usize, core::mem::size_of::<TimeVal>()) {
        return -1;
    }
    unsafe {
        *ts = TimeVal {
            sec: us / 1_000_000,
            usec: us % 1_000_000,
        };
    }
    0
}

/// The three flavours of [`sys_trace`], matching the `TraceRequest` enum of the
/// user library. They arrive in `arg[0]`, so the declaration order of that enum
/// is part of the ABI.
const TRACE_READ: usize = 0;
const TRACE_WRITE: usize = 1;
const TRACE_SYSCALL: usize = 2;

/// Trace the behaviour of the current application.
///
/// * `TRACE_READ` / `TRACE_WRITE`: read or write one byte of the application's
///   own memory. A pointer outside the region handed to the app is refused with
///   `-1` instead of being dereferenced, so a traced address can never fault the
///   kernel. A byte always fits in the non-negative range of `isize`, hence
///   `-1` is unambiguous as "no such address".
/// * `TRACE_SYSCALL`: report how often the current task has invoked the syscall
///   whose id is given in `arg[1]`. Counting happens when the trap is taken (see
///   [`crate::trap::trap_handler`]), which is why the asking call is already
///   included in its own answer. An id this kernel cannot dispatch gives `-1`
///   too, so a stubbed-out implementation cannot be mistaken for a counter that
///   merely has nothing to report.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        TRACE_READ => {
            if !user_addr_valid(id, 1) {
                return -1;
            }
            (unsafe { core::ptr::read_volatile(id as *const u8) }) as isize
        }
        TRACE_WRITE => {
            if !user_addr_valid(id, 1) {
                return -1;
            }
            unsafe {
                core::ptr::write_volatile(id as *mut u8, data as u8);
            }
            0
        }
        TRACE_SYSCALL => {
            if !super::is_supported(id) {
                return -1;
            }
            current_syscall_count(id).map_or(-1, |count| count as isize)
        }
        _ => -1,
    }
}
