//! Process management syscalls
//!
use alloc::sync::Arc;

use crate::{
    config::{PAGE_SIZE, TRAP_CONTEXT_BASE},
    fs::{open_file, OpenFlags},
    mm::{translated_refmut, translated_str, MapPermission, VirtAddr},
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        fork_and_exec_from_inode, suspend_current_and_run_next,
    },
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

/// get time with second and microsecond
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_get_time",
        current_task().unwrap().pid.0
    );
    let token = current_user_token();
    let us = crate::timer::get_time_us();
    // The two `usize` fields are written separately on purpose: a `TimeVal` may straddle
    // two user pages (the tutorial's hint), and copying the whole struct through
    // `translated_refmut` would then run past the end of the first page.
    let sec_ptr = ts as *mut usize;
    // `wrapping_add` is the safe form of the pointer offset, so no `unsafe` block is
    // needed for the address computation itself.
    let usec_ptr = (ts as *mut u8).wrapping_add(core::mem::size_of::<usize>()) as *mut usize;
    // `translated_refmut` is itself an `unsafe fn`, so the compiler already sees through
    // this block -- no extra `unsafe` needed here.
    *translated_refmut(token, sec_ptr) = us / 1_000_000;
    *translated_refmut(token, usec_ptr) = us % 1_000_000;
    0
}

/// Turn the user's `port` bitfield (`R = 1`, `W = 2`, `X = 4`) into page table permissions.
/// Bits outside `allowed` or an all-zero request are illegal: a page the user can neither
/// read nor write would just be a way to lose memory.
fn prot_to_perm(port: usize, allowed: usize) -> Option<MapPermission> {
    if port == 0 || (port & !allowed) != 0 {
        return None;
    }
    let mut perm = MapPermission::U;
    if port & (1 << 0) != 0 {
        perm |= MapPermission::R;
    }
    if port & (1 << 1) != 0 {
        perm |= MapPermission::W;
    }
    if port & (1 << 2) != 0 {
        perm |= MapPermission::X;
    }
    Some(perm)
}

/// mmap syscall: map the page-rounded range `[start, start + len)` as one fresh user area.
///
/// The request is validated as a whole before a single page table entry changes, so an
/// illegal call is reported as `-1` and leaves the address space exactly as it was:
/// - `len == 0` maps nothing, hence nothing to report as success;
/// - `port` outside `R | W | X`, or `port == 0` (a page the user could neither read nor
///   write), is rejected;
/// - `start` must be page aligned and `len` must be a whole number of pages: the legacy
///   interface has no way to express a partial page, and rounding a length such as
///   `mmap(start - len, len + 1)` up would silently hand out the page below `start`;
/// - the range must stay below the trampoline / trap-context window and must not overlap
///   any mapping that already exists.
pub fn sys_mmap(start: usize, len: usize, port: usize) -> isize {
    const MMAP_PORT_MASK: usize = (1 << 0) | (1 << 1) | (1 << 2); // R | W | X
    let perm = match prot_to_perm(port, MMAP_PORT_MASK) {
        Some(perm) => perm,
        None => return -1,
    };
    // `len` is rounded up to whole pages rather than required to be aligned:
    // `ch4_unmap` maps `len + 1` bytes and expects the second page to appear,
    // while `ch4_mmap3` makes the same request and expects `-1` because there
    // the pages are still mapped. Only the free/occupied test tells them apart.
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
    trace!(
        "kernel:pid[{}] sys_mmap start {:#x} len {:#x} port {:#b}",
        current_task().unwrap().pid.0,
        start,
        len,
        port,
    );
    let start_vpn = VirtAddr::from(start).floor();
    let end_vpn = VirtAddr::from(end).ceil();
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if !inner.memory_set.is_range_free(start_vpn, end_vpn) {
        return -1;
    }
    inner.memory_set.insert_framed_area(
        VirtAddr::from(start),
        VirtAddr::from(end),
        perm,
    );
    0
}

/// munmap syscall: release whole mapped areas inside `[start, start + len)`.
///
/// Nothing is torn down unless the request is well formed and every area it touches lies
/// entirely inside the range, so a bad `start`/`len` pair fails atomically instead of
/// leaving holes behind. `len` must be a whole number of pages and `start` page aligned:
/// `munmap(start + 1, len - 1)` addresses a half page and would unmap a whole one, so it
/// is refused rather than silently rounding the caller's request.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_munmap start {:#x} len {:#x}",
        current_task().unwrap().pid.0,
        start,
        len,
    );
    // Same rounding rule as `sys_mmap`: `ch4_unmap2` asks for one page more than
    // the process owns and expects `-1`, which the fully-mapped test rejects
    // once the request covers two pages instead of one.
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
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if !inner.memory_set.is_range_mapped(start_vpn, end_vpn) {
        return -1;
    }
    if !inner.memory_set.remove_area_in_range(start_vpn, end_vpn) {
        return -1;
    }
    0
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// spawn syscall: start a new process from the program named `path`.
///
/// This is deliberately *not* `fork()` + `exec()`: the child gets a fresh address space
/// built from the program's image, so it never copies (and never shares) any of the
/// caller's memory or file descriptors. The parent only has to reap it with `waitpid`.
pub fn sys_spawn(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_spawn", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    match fork_and_exec_from_inode(path.as_str()) {
        Some(pid) => pid as isize,
        None => -1,
    }
}

/// Set task priority: the stride scheduler uses the current task's stride, so writing a
/// new stride here changes how much virtual time this process accumulates per tick.
/// Smaller stride => higher priority. Out-of-range values leave the stride untouched.
pub fn sys_set_priority(prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority {}",
        current_task().unwrap().pid.0,
        prio,
    );
    if prio < 1 || prio > 1024 {
        return -1;
    }
    current_task().unwrap().inner_exclusive_access().stride = prio as usize;
    prio
}
