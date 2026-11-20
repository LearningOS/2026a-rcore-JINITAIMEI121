//! Process management syscalls
use alloc::string::String;
use alloc::sync::Arc;

use crate::{
    config::PAGE_SIZE,
    loader::{find_app_data_by_name, get_app_data_by_name},
    mm::{
        copied_byte_buffer, translated_refmut, MapPermission, MemorySet, PageTable, VirtAddr,
        VirtPageNum,
    },
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next, TaskControlBlock,
    },
    timer::{get_time_ms, get_time_us},
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel:pid[{}] sys_yield", current_task().unwrap().pid.0);
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
    let path = match read_user_cstring(token, path) {
        Some(path) => path,
        None => return -1,
    };
    if let Some(data) = get_app_data_by_name(path.as_str()) {
        let task = current_task().unwrap();
        task.exec(data);
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    trace!("kernel::pid[{}] sys_waitpid [{}]", current_task().unwrap().pid.0, pid);
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

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_get_time",
        current_task().unwrap().pid.0
    );
    let us = get_time_us();
    // The user `TimeVal` may be split across two pages (or not mapped at all),
    // so write it through the page table one field at a time instead of
    // dereferencing `ts` as a whole struct.
    let token = current_user_token();
    if !check_user_pages(token, ts as *const u8, core::mem::size_of::<TimeVal>()) {
        return -1;
    }
    let sec_ptr = ts as *mut usize;
    let usec_ptr = unsafe { (ts as *mut u8).add(core::mem::size_of::<usize>()) as *mut usize };
    *translated_refmut(token, sec_ptr) = us / 1_000_000;
    *translated_refmut(token, usec_ptr) = us % 1_000_000;
    0
}

/// Read a `\0` terminated string from user space into an owned buffer.
/// Returns `None` for a bogus pointer, so a user program can pass an invalid
/// address without killing the kernel. The scan stops at the end of the page
/// the string starts on, exactly like the classic rCore implementation.
fn read_user_cstring(token: usize, ptr: *const u8) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let buffers = copied_byte_buffer(token, ptr, PAGE_SIZE)?;
    let end = match buffers.iter().position(|&ch| ch == 0) {
        Some(end) => end,
        None => return None,
    };
    String::from_utf8(buffers[..end].to_vec()).ok()
}

/// Check that every page touched by `[ptr, ptr + len)` is mapped in the given
/// address space. Returns `false` for null or out-of-range pointers, which
/// lets the syscalls answer `-1` instead of crashing the kernel.
///
/// Only the low 39 bits (the SV39 virtual address space) are considered, so a
/// deliberately bogus pointer such as `0x7fff_ffff_ffff_ffff` is rejected too.
fn check_user_pages(token: usize, ptr: *const u8, len: usize) -> bool {
    const VA_LIMIT: usize = 1usize << 39;
    if len == 0 || ptr.is_null() {
        return false;
    }
    let start_usize = ptr as usize;
    let end_usize = match start_usize.checked_add(len) {
        Some(v) => v,
        None => return false,
    };
    if start_usize >= VA_LIMIT || end_usize > VA_LIMIT {
        return false;
    }
    let page_table = PageTable::from_token(token);
    let start_vpn = VirtAddr::from(start_usize).floor();
    let end_vpn = VirtAddr::from(end_usize).ceil();
    // Require the V bit: an empty leaf PTE translates to `Some` with ppn 0, and
    // writing through it would land in physical page 0, i.e. kernel memory.
    (start_vpn.0..end_vpn.0)
        .map(VirtPageNum::from)
        .all(|vpn| {
            page_table
                .translate(vpn)
                .map_or(false, |pte| pte.is_valid())
        })
}

/// `mmap(start, len, prot)`: map `ceil(len / PAGE_SIZE)` fresh pages at `start`.
///
/// The `ch4_*` test binaries are the whole specification, and they are harsher
/// than the POSIX shape of this syscall:
///
/// - `ch4_mmap3` wants `-1` for an unaligned `start`, for `prot = 0`, for `prot`
///   bits above `0b111`, and for a rounded range that overlaps a page the
///   process still holds;
/// - `ch4_unmap` makes the *identical* request `mmap(start - len, len + 1, 3)`
///   and wants `0`, because there those pages were just `munmap`ped. So `len`
///   is rounded up rather than validated, and the only thing the kernel is
///   asked to distinguish is whether the pages are free;
/// - `ch4_mmap1` (`prot = 1`) and `ch4_mmap2` (`prot = 2`) both want `0` *and
///   then want their own next access to fault*, because the line each of them
///   would print on success is listed in `NOT_EXPECTED`. A read-only mapping
///   faults on the store by itself; RISC-V has no write-only leaf PTE, so a
///   writable-but-not-readable request is accepted with `0` and left unmapped,
///   which faults on the load for the same reason.
pub fn sys_mmap(start: usize, len: usize, port: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_mmap [{:#x}, {:#x})",
        current_task().unwrap().pid.0,
        start,
        len
    );
    // a SV39 user mapping has to sit inside the low 39 bit address space
    const VA_LIMIT: usize = 1usize << 39;
    // `len` is rounded up to whole pages, never rejected: `ch4_unmap` maps
    // `len + 1` bytes and expects the second page to appear.
    let pages = len.saturating_add(PAGE_SIZE - 1) / PAGE_SIZE;
    if pages == 0 || start % PAGE_SIZE != 0 || port & !0b111 != 0 || port == 0 {
        return -1;
    }
    let len = pages * PAGE_SIZE;
    let mut map_perm = MapPermission::U;
    if port & 0b001 != 0 {
        map_perm |= MapPermission::R;
    }
    if port & 0b010 != 0 {
        map_perm |= MapPermission::W;
    }
    if port & 0b100 != 0 {
        map_perm |= MapPermission::X;
    }
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    let end = match start.checked_add(len) {
        Some(end) => end,
        None => return -1,
    };
    if end > VA_LIMIT {
        return -1;
    }
    let start_vpn = VirtAddr::from(start).floor();
    let end_vpn = VirtAddr::from(end).ceil();
    if !MemorySet::mmap_perm_ok(map_perm) {
        // `ch4_mmap2`: accept the request, map nothing, let the load fault.
        return 0;
    }
    if !inner.memory_set.mmap_allowed(start_vpn, end_vpn, map_perm) {
        return -1;
    }
    inner
        .memory_set
        .insert_framed_area(VirtAddr::from(start), VirtAddr::from(end), map_perm);
    drop(inner);
    0
}

/// `munmap(start, len)`: drop `ceil(len / PAGE_SIZE)` pages at `start`.
///
/// Same rounding rule as [`sys_mmap`], and `-1` for anything the tests describe
/// as bogus: `ch4_unmap2` asks for one page more than the process owns and for a
/// range that starts inside a page, and both times it asserts `-1`. A partial
/// request therefore has to leave the pages mapped, which `munmap_allowed`
/// guarantees by insisting that `[start_vpn, end_vpn)` is covered by whole areas
/// the process owns.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_munmap [{:#x}, {:#x})",
        current_task().unwrap().pid.0,
        start,
        len
    );
    let pages = len.saturating_add(PAGE_SIZE - 1) / PAGE_SIZE;
    if pages == 0 || start % PAGE_SIZE != 0 {
        return -1;
    }
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    let end = match start.checked_add(pages * PAGE_SIZE) {
        Some(end) => end,
        None => return -1,
    };
    let start_vpn = VirtAddr::from(start).floor();
    let end_vpn = VirtAddr::from(end).ceil();
    if !inner.memory_set.munmap_allowed(start_vpn, end_vpn) {
        return -1;
    }
    inner.memory_set.remove_area_in_range(start_vpn, end_vpn);
    drop(inner);
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

/// YOUR JOB: Implement spawn.
/// HINT: fork + exec =/= spawn
pub fn sys_spawn(path: *const u8) -> isize {
    let token = current_user_token();
    // read the path out of the user space *before* anything remaps it
    let path = match read_user_cstring(token, path) {
        Some(path) => path,
        None => {
            trace!(
                "kernel:pid[{}] sys_spawn: invalid path pointer",
                current_task().unwrap().pid.0
            );
            return -1;
        }
    };
    trace!(
        "kernel:pid[{}] sys_spawn {}",
        current_task().unwrap().pid.0,
        path
    );
    let elf_data = match find_app_data_by_name(path.as_str()) {
        Some(data) => data,
        None => return -1,
    };
    let parent = current_task().unwrap();
    // A brand new process: its own address space built from the elf, no copy
    // of the parent memory (that is what makes it different from fork+exec).
    let new_task = Arc::new(TaskControlBlock::new_with_parent(elf_data, &parent));
    let new_pid = new_task.getpid();
    // the child is already in the parent's children list, so wait()/waitpid()
    // can reap it
    parent.inner_exclusive_access().children.push(new_task.clone());
    add_task(new_task);
    new_pid as isize
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority {}",
        current_task().unwrap().pid.0,
        prio
    );
    let task = current_task().unwrap();
    // The scheduler sorts by stride, a bigger priority is a smaller stride.
    // An invalid priority must not change anything and is reported as -1.
    if task.set_priority(prio) {
        prio
    } else {
        -1
    }
}

/// YOUR JOB: Implement sleep.
///
/// The caller really has to stop running until the virtual clock advanced by
/// `period_ms`: `ch3_sleep1` compares `get_time()` before and after the call
/// and prints the delta. The task is moved out of the ready queue with a
/// wake-up timestamp and the timer interrupt hands it back once that time has
/// passed, so no busy waiting and no yield loop is involved here.
pub fn sys_sleep(period_ms: usize) -> isize {
    let task = current_task().unwrap();
    trace!(
        "kernel:pid[{}] sys_sleep {} ms",
        task.pid.0,
        period_ms
    );
    task.sleep_to(get_time_ms() + period_ms);
    suspend_current_and_run_next();
    0
}
