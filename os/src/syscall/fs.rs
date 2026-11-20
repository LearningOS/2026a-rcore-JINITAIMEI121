//! File and filesystem-related syscalls
use crate::fs::{open_file, OSInode, OpenFlags, Stat, StatMode, DEV_NUM, ROOT_INODE};
use crate::mm::{translated_byte_buffer, translated_str, UserBuffer};
use crate::task::{current_task, current_user_token};

pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_write", current_task().unwrap().pid.0);
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        if !file.writable() {
            return -1;
        }
        let file = file.clone();
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        file.write(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}

pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_read", current_task().unwrap().pid.0);
    let token = current_user_token();
    let task = current_task().unwrap();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        let file = file.clone();
        if !file.readable() {
            return -1;
        }
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        trace!("kernel: sys_read .. file.read");
        file.read(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}

pub fn sys_open(path: *const u8, flags: u32) -> isize {
    trace!("kernel:pid[{}] sys_open", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(inode) = open_file(path.as_str(), OpenFlags::from_bits(flags).unwrap()) {
        let mut inner = task.inner_exclusive_access();
        let fd = inner.alloc_fd();
        inner.fd_table[fd] = Some(inode);
        fd as isize
    } else {
        -1
    }
}

pub fn sys_close(fd: usize) -> isize {
    trace!("kernel:pid[{}] sys_close", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if inner.fd_table[fd].is_none() {
        return -1;
    }
    inner.fd_table[fd].take();
    0
}

/// unlinkat syscall
pub fn sys_unlinkat(name: *const u8) -> isize {
    trace!(
        "kernel:pid[{}] sys_unlinkat",
        current_task().unwrap().pid.0
    );
    let token = current_user_token();
    let path = translated_str(token, name);
    if path.is_empty() || path.starts_with('/') || path.contains('/') || path == "." {
        return -1;
    }
    // Look the dirent up first, so the inode being detached is known before the entry
    // disappears. `find()` hands out a fresh `Arc<Inode>` per call, so no in-memory file
    // observes the shrink below while it is still reachable by name.
    let target = match ROOT_INODE.find(path.as_str()) {
        Some(inode) => inode,
        None => return -1,
    };
    let nlink = ROOT_INODE.count_dirents_to(target.id());
    if !ROOT_INODE.remove_dirent(path.as_str()) {
        return -1;
    }
    // POSIX: only the last link releases the data blocks. The dirent is gone now, so a
    // remaining count of zero means nobody can reach this inode by name any more; if some
    // descriptor still holds it open, that object owns the data and its `Drop` performs the
    // release when the file is finally closed (see `impl Drop for OSInode`).
    if nlink <= 1 && ROOT_INODE.count_dirents_to(target.id()) == 0 {
        target.free_blocks();
    }
    0
}

/// fstat syscall
pub fn sys_fstat(fd: usize, st: *mut Stat) -> isize {
    trace!("kernel:pid[{}] sys_fstat", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let token = current_user_token();
    let inner = task.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    let file = match &inner.fd_table[fd] {
        Some(file) => file.clone(),
        None => return -1,
    };
    drop(inner);
    // Only an open disk inode can be stat'ed; stdin/stdout have no inode behind them.
    let os_inode = match file.downcast_ref::<OSInode>() {
        Some(os_inode) => os_inode,
        None => return -1,
    };
    // `ino` comes from the open file itself, not from a name lookup, so a file whose
    // directory entries were all unlinked while it stayed open still reports its inode.
    let ino = os_inode.ino();
    let mode = if os_inode.is_dir() {
        StatMode::DIR
    } else {
        StatMode::FILE
    };
    let stat = Stat::new(
        DEV_NUM,
        ino as u64,
        mode,
        ROOT_INODE.count_dirents_to(ino),
        os_inode.size() as u64,
    );
    debug_assert_eq!(core::mem::size_of::<Stat>(), Stat::USER_SIZE);
    // Copy exactly the prefix the caller allocated: `user_lib::Stat` is 32 bytes and so
    // is the kernel one, but assigning the whole struct through a reference would still
    // be wrong if either layout ever grows.
    let buf = translated_byte_buffer(token, st as *const u8, Stat::USER_SIZE);
    assert_eq!(
        buf.iter().map(|b| b.len()).sum::<usize>(),
        Stat::USER_SIZE,
        "sys_fstat: user stat buffer shorter than Stat::USER_SIZE"
    );
    let mut src: &[u8] = unsafe {
        core::slice::from_raw_parts(&stat as *const Stat as *const u8, Stat::USER_SIZE)
    };
    for dst in buf {
        let (head, rest) = src.split_at(dst.len());
        dst.copy_from_slice(head);
        src = rest;
    }
    0
}

/// linkat syscall
pub fn sys_linkat(old_name: *const u8, new_name: *const u8) -> isize {
    trace!(
        "kernel:pid[{}] sys_linkat",
        current_task().unwrap().pid.0
    );
    let token = current_user_token();
    let old_path = translated_str(token, old_name);
    let new_path = translated_str(token, new_name);
    for path in [&old_path, &new_path] {
        if path.is_empty() || path.starts_with('/') || path.contains('/') || path == "." {
            return -1;
        }
    }
    let inode = match ROOT_INODE.find(old_path.as_str()) {
        Some(inode) => inode,
        None => return -1,
    };
    if inode.is_dir() {
        // hard links to directories are what turn a directory tree into a graph
        return -1;
    }
    // A dirent may not alias two names, and appending one is all a hard link is:
    // the nlink reported by fstat is derived from the directory, so it grows by itself.
    if ROOT_INODE.find(new_path.as_str()).is_some() {
        return -1;
    }
    match ROOT_INODE.link(&inode, new_path.as_str()) {
        Some(_) => 0,
        None => -1,
    }
}
