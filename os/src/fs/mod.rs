//! File trait & inode(dir, file, pipe, stdin, stdout)

mod inode;
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
    /// Identity for downcasting. `sys_fstat` needs the concrete `OSInode` behind an
    /// `Arc<dyn File>` slot of the fd table. The kernel never reinterprets a file
    /// object, so exposing `self` as `&dyn Any` is sound.
    fn as_any(&self) -> &(dyn core::any::Any + Send + Sync);
}

impl dyn File + Send + Sync {
    /// Recover the concrete type of a `File`.
    ///
    /// `core::any::Any` deliberately has no `downcast_ref` for an unsized receiver, so the
    /// identity is taken from `as_any()` and compared by `TypeId` -- the same thing the
    /// std impl does for sized types. The lifetime stays tied to `&self`, so no reference
    /// outlives the file object.
    pub fn downcast_ref<T: core::any::Any>(&self) -> Option<&T> {
        if self.as_any().type_id() == core::any::TypeId::of::<T>() {
            // SAFETY: the `TypeId` check above proves the dyn data pointer really points
            // at a `T`, and the vtable (Send + Sync) is unchanged by the cast.
            Some(unsafe { &*(self as *const dyn File as *const T) })
        } else {
            None
        }
    }
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
    /// file size in bytes
    pub size: u64,
}

impl Stat {
    /// Build the kernel view of an open file.
    pub fn new(dev: u64, ino: u64, mode: StatMode, nlink: u32, size: u64) -> Self {
        Self {
            dev,
            ino,
            mode,
            nlink,
            size,
        }
    }
}

impl Stat {
    /// Byte size of the *user-side* `Stat` object that `sys_fstat` fills in.
    ///
    /// `user_lib::Stat` is `{ dev: u64, ino: u64, mode: StatMode /* u32 bitflags */,
    /// nlink: u32, size: u64 }` = 8 + 8 + 4 + 4 + 8 = **32** bytes, which is exactly the
    /// prefix of the kernel struct above (`mode` is a `u32` bitflag there too). Only this
    /// prefix is copied to user space: writing more would scribble past the caller's
    /// object, and the assertion in `sys_fstat` fires if the layout assumption breaks.
    pub const USER_SIZE: usize = 32;
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

/// Device number reported by `fstat`. There is exactly one filesystem, the easy-fs
/// volume on the single virtio-blk device, so it is a constant.
pub const DEV_NUM: u64 = 0;

pub use inode::{list_apps, open_file, OSInode, OpenFlags};
pub(crate) use inode::ROOT_INODE;
pub use stdio::{Stdin, Stdout};
