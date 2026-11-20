//! `Arc<Inode>` -> `OSInodeInner`: In order to open files concurrently
//! we need to wrap `Inode` into `Arc`,but `Mutex` in `Inode` prevents
//! file systems from being accessed simultaneously
//!
//! `UPSafeCell<OSInodeInner>` -> `OSInode`: for static `ROOT_INODE`,we
//! need to wrap `OSInodeInner` into `UPSafeCell`
use super::File;
use crate::drivers::BLOCK_DEVICE;
use crate::mm::UserBuffer;
use crate::sync::UPSafeCell;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bitflags::*;
use easy_fs::{EasyFileSystem, Inode};
use lazy_static::*;

/// inode in memory
/// A wrapper around a filesystem inode
/// to implement File trait atop
pub struct OSInode {
    readable: bool,
    writable: bool,
    inner: UPSafeCell<OSInodeInner>,
}
/// The OS inode inner in 'UPSafeCell'
pub struct OSInodeInner {
    offset: usize,
    inode: Arc<Inode>,
}

impl OSInode {
    /// create a new inode in memory
    pub fn new(readable: bool, writable: bool, inode: Arc<Inode>) -> Self {
        Self {
            readable,
            writable,
            inner: unsafe { UPSafeCell::new(OSInodeInner { offset: 0, inode }) },
        }
    }
    /// read all data from the inode
    pub fn read_all(&self) -> Vec<u8> {
        let mut inner = self.inner.exclusive_access();
        let mut buffer: Vec<u8> = Vec::with_capacity(512);
        buffer.resize(512, 0);
        let mut v: Vec<u8> = Vec::new();
        loop {
            let len = inner.inode.read_at(inner.offset, &mut buffer);
            if len == 0 {
                break;
            }
            inner.offset += len;
            v.extend_from_slice(&buffer[..len]);
        }
        v
    }
    /// The inode number of this open file. It is a property of the underlying disk
    /// inode, so it stays valid even after every directory entry that named it has been
    /// unlinked -- exactly what `fstat` needs to report while a deleted-but-open file is
    /// still being written to.
    pub fn ino(&self) -> u32 {
        self.inner.exclusive_access().inode.id()
    }
    /// Whether this open file is a directory. The kernel never creates one, but `fstat`
    /// must not report a directory as a regular file if the image ever contains one.
    pub fn is_dir(&self) -> bool {
        self.inner.exclusive_access().inode.is_dir()
    }
    /// Length of the file in bytes, taken from the disk inode rather than from a name
    /// lookup, so it stays meaningful once every dirent for the file is gone.
    pub fn size(&self) -> usize {
        self.inner.exclusive_access().inode.size()
    }
    /// Snapshot of the whole file content. Only used by `sys_unlinkat`, which needs to
    /// tell an already-detached (and possibly re-used) inode number apart from the live
    /// dirent of the same name before it destroys data.
    pub fn read_all_content(&self) -> Vec<u8> {
        self.inner.exclusive_access().inode.read_all()
    }
    /// How many blocks (data + indirect tables) the underlying disk inode owns. A file
    /// that was already unlinked and truncated reads 0, which is what lets `sys_unlinkat`
    /// refuse to unlink a name that no longer refers to the descriptor's inode.
    pub fn block_count(&self) -> usize {
        self.inner.exclusive_access().inode.block_count()
    }
}

/// The last reference to an open file owns its data blocks. easy-fs has no on-disk link
/// count and no in-memory inode refcount, so this `Arc` *is* the reference: when a file
/// was unlinked while still open, closing it here is the first (and only) safe point to
/// release the blocks. Releasing them during the unlink itself would be wrong twice over
/// -- other descriptors would keep writing into freed blocks, and because inode ids come
/// from a bitmap the id could already have been re-used by a different live file.
impl Drop for OSInode {
    fn drop(&mut self) {
        // Only a writable handle may change anything on disk; read-only descriptors never
        // truncate. `blocks != 0` short-circuits the common case without touching the
        // allocator, so this stays cheap for stdin/stdout-style objects.
        if !self.writable {
            return;
        }
        let (id, blocks) = {
            let inner = self.inner.exclusive_access();
            (inner.inode.id(), inner.inode.block_count())
        };
        if blocks != 0 && ROOT_INODE.count_dirents_to(id) == 0 {
            // The borrow above is gone, so the filesystem lock can be taken again.
            let inner = self.inner.exclusive_access();
            inner.inode.free_blocks();
        }
    }
}

lazy_static! {
    /// The root directory of the easy-fs volume, opened once at boot. It is the handle
    /// every path-based syscall (`find`/`create`/`link`/`remove_dirent`) goes through.
    pub static ref ROOT_INODE: Arc<Inode> = {
        let efs = EasyFileSystem::open(BLOCK_DEVICE.clone());
        Arc::new(EasyFileSystem::root_inode(&efs))
    };
}

/// List all apps in the root directory
pub fn list_apps() {
    println!("/**** APPS ****");
    for app in ROOT_INODE.ls() {
        println!("{}", app);
    }
    println!("**************/");
}

bitflags! {
    ///  The flags argument to the open() system call is constructed by ORing together zero or more of the following values:
    pub struct OpenFlags: u32 {
        /// readyonly
        const RDONLY = 0;
        /// writeonly
        const WRONLY = 1 << 0;
        /// read and write
        const RDWR = 1 << 1;
        /// create new file
        const CREATE = 1 << 9;
        /// truncate file size to 0
        const TRUNC = 1 << 10;
    }
}

impl OpenFlags {
    /// Do not check validity for simplicity
    /// Return (readable, writable)
    pub fn read_write(&self) -> (bool, bool) {
        if self.is_empty() {
            (true, false)
        } else if self.contains(Self::WRONLY) {
            (false, true)
        } else {
            (true, true)
        }
    }
}

/// Open a file
pub fn open_file(name: &str, flags: OpenFlags) -> Option<Arc<OSInode>> {
    let (readable, writable) = flags.read_write();
    if flags.contains(OpenFlags::CREATE) {
        if let Some(inode) = ROOT_INODE.find(name) {
            // clear size
            inode.clear();
            Some(Arc::new(OSInode::new(readable, writable, inode)))
        } else {
            // create file
            ROOT_INODE
                .create(name)
                .map(|inode| Arc::new(OSInode::new(readable, writable, inode)))
        }
    } else {
        ROOT_INODE.find(name).map(|inode| {
            if flags.contains(OpenFlags::TRUNC) {
                inode.clear();
            }
            Arc::new(OSInode::new(readable, writable, inode))
        })
    }
}

impl File for OSInode {
    fn as_any(&self) -> &(dyn core::any::Any + Send + Sync) {
        self
    }
    fn readable(&self) -> bool {
        self.readable
    }
    fn writable(&self) -> bool {
        self.writable
    }
    fn read(&self, mut buf: UserBuffer) -> usize {
        let mut inner = self.inner.exclusive_access();
        let mut total_read_size = 0usize;
        for slice in buf.buffers.iter_mut() {
            let read_size = inner.inode.read_at(inner.offset, *slice);
            if read_size == 0 {
                break;
            }
            inner.offset += read_size;
            total_read_size += read_size;
        }
        total_read_size
    }
    fn write(&self, buf: UserBuffer) -> usize {
        let mut inner = self.inner.exclusive_access();
        let mut total_write_size = 0usize;
        for slice in buf.buffers.iter() {
            let write_size = inner.inode.write_at(inner.offset, *slice);
            assert_eq!(write_size, slice.len());
            inner.offset += write_size;
            total_write_size += write_size;
        }
        total_write_size
    }
}
