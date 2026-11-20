use crate::{
    config::{PAGE_SIZE, TRAP_CONTEXT_BASE},
    fs::{open_file, OpenFlags},
    mm::{
        translated_ref, translated_refmut, translated_str, MapPermission, MAX_MAP_SIZE, VirtAddr,
    },
    task::{
        current_process, current_task, current_user_token, exit_current_and_run_next, pid2process,
        suspend_current_and_run_next, ProcessControlBlock, SignalFlags, MAX_PRIORITY,
        MIN_MMAP_ADDR, MIN_PRIORITY,
    },
};
use alloc::{string::String, sync::Arc, vec::Vec};

/// The lowest virtual address `sys_mmap` is allowed to hand out. Below it lie
/// the program image and the (growing) data segment used by `sys_sbrk`.
const MMAP_BASE: usize = MIN_MMAP_ADDR;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// exit syscall
///
/// exit the current task and run the next task in task list
pub fn sys_exit(exit_code: i32) -> ! {
    trace!(
        "kernel:pid[{}] sys_exit",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}
/// yield syscall
pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}
/// getpid syscall
pub fn sys_getpid() -> isize {
    trace!(
        "kernel: sys_getpid pid:{}",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    current_task().unwrap().process.upgrade().unwrap().getpid() as isize
}
/// fork child process syscall
pub fn sys_fork() -> isize {
    trace!(
        "kernel:pid[{}] sys_fork",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let current_process = current_process();
    let new_process = current_process.fork();
    let new_pid = new_process.getpid();
    // modify trap context of new_task, because it returns immediately after switching
    let new_process_inner = new_process.inner_exclusive_access();
    let task = new_process_inner.tasks[0].as_ref().unwrap();
    let trap_cx = task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    new_pid as isize
}
/// exec syscall
pub fn sys_exec(path: *const u8, mut args: *const usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_exec",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let token = current_user_token();
    let path = translated_str(token, path);
    let mut args_vec: Vec<String> = Vec::new();
    loop {
        let arg_str_ptr = *translated_ref(token, args);
        if arg_str_ptr == 0 {
            break;
        }
        args_vec.push(translated_str(token, arg_str_ptr as *const u8));
        unsafe {
            args = args.add(1);
        }
    }
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let process = current_process();
        let argc = args_vec.len();
        process.exec(all_data.as_slice(), args_vec);
        // return argc because cx.x[10] will be covered with it later
        argc as isize
    } else {
        -1
    }
}

/// waitpid syscall
///
/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let process = current_process();
    // find a child process

    let mut inner = process.inner_exclusive_access();
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
        p.inner_exclusive_access().is_zombie && (pid == -1 || pid as usize == p.getpid())
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

/// kill syscall
pub fn sys_kill(pid: usize, signal: u32) -> isize {
    trace!(
        "kernel:pid[{}] sys_kill",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    if let Some(process) = pid2process(pid) {
        if let Some(flag) = SignalFlags::from_bits(signal) {
            process.inner_exclusive_access().signals |= flag;
            0
        } else {
            -1
        }
    } else {
        -1
    }
}

/// get_time syscall
///
/// Writes the current wall-clock time into the user `TimeVal` structure. The
/// pointer is translated through the *current* address space, so a bad pointer
/// only kills this task (SIGSEGV) instead of faulting inside the kernel.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_get_time",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let us = crate::timer::get_time_us();
    let secs = us / 1_000_000;
    let usecs = us % 1_000_000;
    let token = current_user_token();
    let ts_ref = translated_refmut(token, ts);
    unsafe {
        core::ptr::write_volatile(&mut ts_ref.sec, secs);
        core::ptr::write_volatile(&mut ts_ref.usec, usecs);
    }
    0
}

/// mmap syscall
///
/// Map `len` bytes of anonymous private memory into the current address space.
/// When `addr` is non-zero it is used as a hint; the returned address is always
/// page aligned and zero filled.
pub fn sys_mmap(addr: usize, len: usize, port: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_mmap addr={:#x} len={:#x} port={:#x}",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        addr,
        len,
        port
    );
    // sanity checks: no negative/overflowing length, permission fits in R/W/X
    if len == 0 || len > MAX_MAP_SIZE {
        return -1;
    }
    if port & !0b111 != 0 {
        return -1;
    }
    let mut map_perm = MapPermission::U;
    if port & 1 != 0 {
        map_perm |= MapPermission::R;
    }
    if port & 2 != 0 {
        map_perm |= MapPermission::W;
    }
    if port & 4 != 0 {
        map_perm |= MapPermission::X;
    }
    let start_aligned = addr & (PAGE_SIZE - 1) == 0;
    let process = current_process();
    let mut inner = process.inner_exclusive_access();
    let pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    let virt_addr = if addr != 0 && start_aligned {
        VirtAddr::from(addr)
    } else {
        match inner.find_free_va(MMAP_BASE, pages) {
            Some(va) => va,
            None => return -1,
        }
    };
    let end_va: VirtAddr = (usize::from(virt_addr) + pages * PAGE_SIZE).into();
    // refuse to map over something that already exists, or outside the user
    // window reserved for mmap / thread stacks
    if usize::from(virt_addr) < MMAP_BASE || usize::from(end_va) > TRAP_CONTEXT_BASE {
        return -1;
    }
    if inner.memory_set.is_mapped(virt_addr, end_va) {
        return -1;
    }
    if inner.memory_set.map_pages(virt_addr, end_va, map_perm).is_none() {
        return -1;
    }
    drop(inner);
    usize::from(virt_addr) as isize
}

/// munmap syscall
///
/// Unmap the `len` bytes starting at the page aligned `start`.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_munmap",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    if len == 0 || len > MAX_MAP_SIZE || start % PAGE_SIZE != 0 {
        return -1;
    }
    let pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    let start_va: VirtAddr = start.into();
    let end_va: VirtAddr = (start + pages * PAGE_SIZE).into();
    let process = current_process();
    let mut inner = process.inner_exclusive_access();
    if usize::from(start_va) < MMAP_BASE || usize::from(end_va) > TRAP_CONTEXT_BASE {
        return -1;
    }
    inner.memory_set.unmap_area(start_va, end_va);
    drop(inner);
    0
}

/// spawn syscall
///
/// Create a new process from the elf stored in `path`, without touching the
/// current one (no fork/exec round-trip through the user address space).
pub fn sys_spawn(path: *const u8) -> isize {
    trace!(
        "kernel:pid[{}] sys_spawn",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let token = current_user_token();
    let path = translated_str(token, path);
    let inode = open_file(path.as_str(), OpenFlags::RDONLY);
    if inode.is_none() {
        return -1;
    }
    let all_data = inode.unwrap().read_all();
    let new_pid = ProcessControlBlock::new(all_data.as_slice()).getpid();
    new_pid as isize
}

/// set priority syscall
///
/// The scheduler is a stride scheduler: a task with priority `p` advances its
/// stride by `STRIDE / p` per quantum, so `p` must be positive.
pub fn sys_set_priority(prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    if prio < MIN_PRIORITY as isize || prio > MAX_PRIORITY as isize {
        return -1;
    }
    let task = current_task().unwrap();
    task.set_priority(prio as usize);
    prio
}
