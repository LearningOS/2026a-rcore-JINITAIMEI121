//! Process management syscalls
use crate::config::{PAGE_SIZE, TRAP_CONTEXT_BASE};
use crate::mm::{
    read_user_bytes, try_translated_refmut, write_user_bytes, MapPermission, VirtAddr,
};
use crate::task::{
    change_program_brk, current_user_token, exit_current_and_run_next, get_syscall_count,
    suspend_current_and_run_next, with_current_task,
};
use crate::timer::get_time_us;

/// `TraceRequest` variants of the user side `sys_trace`, see
/// `user_lib::TraceRequest`. The discriminants must stay in sync with that
/// `#[repr(C)] enum`: Read = 0, Write = 1, Syscall = 2.
const TRACE_READ: usize = 0;
const TRACE_WRITE: usize = 1;
const TRACE_SYSCALL: usize = 2;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    let token = current_user_token();
    // The user `TimeVal` may straddle two pages, so the two fields are written
    // one at a time through the page table instead of dereferencing `ts`.
    // `try_translated_refmut` also refuses an unmapped/non-writable address,
    // which must not be able to fault the kernel.
    let sec_ptr = ts as *mut usize;
    let usec_ptr = unsafe { (ts as *mut u8).add(core::mem::size_of::<usize>()) as *mut usize };
    let sec = match try_translated_refmut(token, sec_ptr) {
        Some(sec) => sec,
        None => return -1,
    };
    let usec = match try_translated_refmut(token, usec_ptr) {
        Some(usec) => usec,
        None => return -1,
    };
    *sec = us / 1_000_000;
    *usec = us % 1_000_000;
    0
}

/// TODO: Finish sys_trace to pass testcases
/// HINT: You might reimplement it with virtual memory management.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        // read one byte of the current user address space
        TRACE_READ => match read_user_bytes(current_user_token(), id as *const u8, 1) {
            Some(buf) => buf[0] as isize,
            None => -1,
        },
        // write one byte into the current user address space
        TRACE_WRITE => {
            if write_user_bytes(current_user_token(), id as *mut u8, &[data as u8]) {
                0
            } else {
                -1
            }
        }
        // how often has the current task called syscall `id`?
        TRACE_SYSCALL => match get_syscall_count(id) {
            Some(count) => count as isize,
            None => -1,
        },
        _ => -1,
    }
}

/// Is `prot` a legal mmap permission mask? Only R/W/X are meaningful, and
/// `W` without `R` is rejected: RV64 requires `R` whenever `W` or `X` is set
/// in a leaf PTE (sv39 spec, "reserved for future use" otherwise), and the
/// tutorial tests expect such a mapping to fail.
fn prot_to_perm(prot: usize) -> Option<MapPermission> {
    const PROT_READ: usize = 1;
    const PROT_WRITE: usize = 2;
    const PROT_EXEC: usize = 4;
    if prot & !(PROT_READ | PROT_WRITE | PROT_EXEC) != 0 || prot == 0 {
        return None;
    }
    let mut perm = MapPermission::U;
    if prot & PROT_READ != 0 {
        perm |= MapPermission::R;
    }
    if prot & PROT_WRITE != 0 {
        perm |= MapPermission::W;
    }
    if prot & PROT_EXEC != 0 {
        perm |= MapPermission::X;
    }
    Some(perm)
}

// YOUR JOB: Implement mmap.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!(
        "kernel: sys_mmap [{:#x}, {:#x}), prot = {:#x}",
        start,
        start + len,
        prot
    );
    let perm = match prot_to_perm(prot) {
        Some(perm) => perm,
        None => return -1,
    };
    // `len` is rounded up to whole pages, not required to be page aligned:
    // `ch4_unmap` maps `len + 1` bytes and expects the second page to appear,
    // while `ch4_mmap3` makes the same request and expects `-1` because there
    // the pages are still mapped. Only the free/occupied test distinguishes them.
    let pages = len.saturating_add(PAGE_SIZE - 1) / PAGE_SIZE;
    if pages == 0 || start % PAGE_SIZE != 0 {
        return -1;
    }
    let len = pages * PAGE_SIZE;
    let end = match start.checked_add(len) {
        Some(end) => end,
        None => return -1,
    };
    // never map over the trampoline / TrapContext region at the top
    if end > TRAP_CONTEXT_BASE {
        return -1;
    }
    let start_vpn = VirtAddr::from(start).floor();
    let end_vpn = VirtAddr::from(end).ceil();
    with_current_task(|task| {
        if !task.memory_set.is_range_free(start_vpn, end_vpn) {
            return -1;
        }
        task.memory_set
            .insert_framed_area(VirtAddr::from(start), VirtAddr::from(end), perm);
        0
    })
}

// YOUR JOB: Implement munmap.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!(
        "kernel: sys_munmap [{:#x}, {:#x})",
        start,
        start.checked_add(len).unwrap_or(usize::MAX)
    );
    // Same rounding rule as `sys_mmap`. `ch4_unmap2` asks for one page more
    // than the process owns and expects `-1`, which the fully-mapped test
    // below rejects once the request covers two pages instead of one.
    let pages = len.saturating_add(PAGE_SIZE - 1) / PAGE_SIZE;
    if pages == 0 || start % PAGE_SIZE != 0 {
        return -1;
    }
    let end = match start.checked_add(pages * PAGE_SIZE) {
        Some(end) => end,
        None => return -1,
    };
    if end > TRAP_CONTEXT_BASE {
        return -1;
    }
    let start_vpn = VirtAddr::from(start).floor();
    let end_vpn = VirtAddr::from(end).ceil();
    with_current_task(|task| {
        if !task.memory_set.is_range_mapped(start_vpn, end_vpn) {
            return -1;
        }
        if !task.memory_set.remove_area_in_range(start_vpn, end_vpn) {
            return -1;
        }
        0
    })
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
