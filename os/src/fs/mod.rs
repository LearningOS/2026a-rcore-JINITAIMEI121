//! File trait & inode(dir, file, pipe, stdin, stdout)

mod inode;
mod pipe;
mod stdio;

use crate::mm::UserBuffer;

/// trait File for all file types
pub trait File: Send + Sync {
    /// the file readable?
    fn readable(&self) -> bool;
    /// the file writable?
    fn writable(&self) -> bool;
    /// read from the file to buf, return the number of bytes read
    fn read(&self, buf: UserBuffer) -> usize;
    /// write to the file from buf, return the number of bytes written
    fn write(&self, buf: UserBuffer) -> usize;
}

/// The stat of a inode
#[repr(C)]
#[derive(Debug)]
pub struct Stat {
    /// ID of device containing file
    pub dev: u64,
    /// inode number
    pub ino: u64,
    /// file type and mode
    pub mode: StatMode,
    /// number of hard links
    pub nlink: u32,
    /// unused pad
    pad: [u64; 7],
}

bitflags! {
    /// The mode of a inode
    /// whether a directory or a file
    pub struct StatMode: u32 {
        /// null
        const NULL  = 0;
        /// directory
        const DIR   = 0o040000;
        /// ordinary regular file
        const FILE  = 0o100000;
    }
}

pub use inode::{list_apps, open_file, OSInode, OpenFlags};
pub use pipe::{make_pipe, Pipe};
pub use stdio::{Stdin, Stdout};

use crate::drivers::BLOCK_DEVICE;
use alloc::sync::Arc;
use easy_fs::{EasyFileSystem, Inode, Mutex};
use lazy_static::*;

lazy_static! {
    /// The single easy-fs instance every file related syscall works on. It is
    /// the same filesystem `ROOT_INODE` (in [`inode`]) reads its entries from.
    pub static ref EASY_FS: Arc<Mutex<EasyFileSystem>> =
        EasyFileSystem::open(BLOCK_DEVICE.clone());
    /// The root directory of the filesystem.
    pub static ref ROOT_DIR: Arc<Inode> =
        Inode::new_with_id(Arc::clone(&EASY_FS), BLOCK_DEVICE.clone(), 0);
}

/// Fill a [`Stat`] from an easy-fs inode.
pub fn efs_stat(inode: &Arc<Inode>) -> Stat {
    let mode = if inode.is_dir() {
        StatMode::DIR
    } else {
        StatMode::FILE
    };
    Stat {
        dev: 0,
        ino: inode.id() as u64,
        mode,
        nlink: inode.nlink(),
        pad: [0; 7],
    }
}

/// Create a hard link called `new_name` to the file `old_name`.
pub fn link_file(old_name: &str, new_name: &str) -> isize {
    let inode_id = match ROOT_DIR.lookup_inode_id(old_name) {
        Some(id) => id,
        None => return -1,
    };
    // directories must not be hard linked
    let target = Inode::new_with_id(Arc::clone(&EASY_FS), BLOCK_DEVICE.clone(), inode_id);
    if target.is_dir() {
        return -1;
    }
    if ROOT_DIR.link(new_name, inode_id).is_some() {
        0
    } else {
        -1
    }
}

/// Remove the directory entry `name`, releasing the inode when this was its
/// last hard link.
pub fn unlink_file(name: &str) -> isize {
    if ROOT_DIR.unlink(name) {
        0
    } else {
        -1
    }
}

