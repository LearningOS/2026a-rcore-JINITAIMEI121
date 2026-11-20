use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ, DISK_INODE_SZ, BLOCK_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};
/// Virtual filesystem layer over easy-fs
pub struct Inode {
    block_id: usize,
    block_offset: usize,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        block_id: u32,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            block_id: block_id as usize,
            block_offset,
            fs,
            block_device,
        }
    }
    /// Call a function over a disk inode to read it
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if dirent.name() == name {
                return Some(dirent.inode_id() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        let fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode).map(|inode_id| {
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                Arc::new(Self::new(
                    block_id,
                    block_offset,
                    self.fs.clone(),
                    self.block_device.clone(),
                ))
            })
        })
    }
    /// Increase the size of a disk inode
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    /// Create inode under current inode by name
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        let mut fs = self.fs.lock();
        let op = |root_inode: &DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        if self.read_disk_inode(op).is_some() {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
            });
        self.modify_disk_inode(|root_inode| {
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(name, new_inode_id);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            block_id,
            block_offset,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }
    /// Create a hard link called `name` in this (directory) inode that refers to the
    /// existing `target` inode. Unlike `create()` no inode is allocated and the target's
    /// data blocks are left untouched -- only one extra directory entry is appended, and
    /// it carries the *target's* inode id, which is what makes both names resolve to the
    /// same file. The target's link count grows with it. Returns `None` when `name` is
    /// already used.
    pub fn link(&self, target: &Arc<Inode>, name: &str) -> Option<Arc<Inode>> {
        let mut fs = self.fs.lock();
        let exists =
            self.read_disk_inode(|dir_inode| self.find_inode_id(name, dir_inode).is_some());
        if exists {
            return None;
        }
        // The dirent has to point at the target itself. Handing out a freshly allocated
        // inode id here would create an unrelated empty file that shares nothing with
        // `target`, so writing through one name would not be visible through the other.
        let target_inode_id = target.id_with_fs(&fs);
        self.modify_disk_inode(|dir_inode| {
            assert!(dir_inode.is_dir());
            // append the dirent at the end of the directory
            let file_count = (dir_inode.size as usize) / DIRENT_SZ;
            let new_size = ((file_count + 1) * DIRENT_SZ) as u32;
            self.increase_size(new_size, dir_inode, &mut fs);
            let dirent = DirEntry::new(name, target_inode_id);
            dir_inode.write_at(file_count * DIRENT_SZ, dirent.as_bytes(), &self.block_device);
        });
        block_cache_sync_all();
        Some(target.clone())
    }
    /// List inodes under current inode
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                v.push(String::from(dirent.name()));
            }
            v
        })
    }
    /// Whether this inode is a directory.
    pub fn is_dir(&self) -> bool {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.is_dir())
    }
    /// Total size of the file data in bytes. `fstat` reports it without going through a
    /// name, so a deleted-but-still-open file keeps reporting its real length.
    pub fn size(&self) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.size as usize)
    }
    /// Number of blocks (data *and* indirect tables) this inode currently owns.
    pub fn block_count(&self) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| DiskInode::total_blocks(disk_inode.size) as usize)
    }
    /// Read the whole content of a *file* inode into a fresh buffer.
    ///
    /// Used by `sys_linkat`, which has to make sure that a name it is about to detach is
    /// really the one the caller opened: easy-fs dirents carry no generation counter, so
    /// after a mass create/unlink cycle an old inode id can be re-used by a new file.
    /// The kernel refuses to unlink in that case rather than corrupting the new file.
    pub fn read_all(&self) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 512];
        let mut offset = 0usize;
        loop {
            let len = self.read_at(offset, &mut chunk);
            if len == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..len]);
            offset += len;
        }
        buf
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }
    /// Number of directory entries in this (directory) inode that point at `inode_id`.
    ///
    /// This is how the hard link count is obtained: it is derived from the root
    /// directory itself instead of being cached in the inode, so it stays correct for
    /// images produced by an unmodified `easy-fs-fuse` and needs no on-disk format
    /// change. A file with no dirent pointing at it reports 0 links.
    pub fn count_dirents_to(&self, inode_id: u32) -> u32 {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            assert!(disk_inode.is_dir());
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut count = 0u32;
            let mut dirent = DirEntry::empty();
            for i in 0..file_count {
                assert_eq!(
                    disk_inode.read_at(
                        DIRENT_SZ * i,
                        dirent.as_bytes_mut(),
                        &self.block_device,
                    ),
                    DIRENT_SZ,
                );
                if dirent.inode_id() == inode_id {
                    count += 1;
                }
            }
            count
        })
    }
    /// Remove the directory entry called `name` from this (directory) inode.
    ///
    /// Dirents are stored densely, so the removed slot is refilled by shifting the tail
    /// one position left and shrinking the directory by exactly one entry. The entries
    /// live in *data blocks* that belong to the directory inode, which other inodes may
    /// not own, so the blocks cannot simply be freed here:
    ///
    /// - a block that stays inside `[0, total_blocks)` after the shrink keeps its slot id
    ///   and is rewritten in place (the shifted entries plus a zeroed last slot);
    /// - only a block that leaves the inode's range entirely is returned to the allocator,
    ///   and then through `deallocate_block_id`, which zeroes it and clears the pointer
    ///   chain that led to it.
    ///
    /// Returns `false` when `name` is not present.
    pub fn remove_dirent(&self, name: &str) -> bool {
        let mut fs = self.fs.lock();
        // Locate the entry and snapshot every entry behind it in one pass. The snapshot
        // must be taken while the directory still has its old size, because both
        // `read_at`/`write_at` clamp to `size`.
        struct Shift {
            idx: usize,
            file_count: usize,
            tail: Vec<DirEntry>,
        }
        let shift = self.read_disk_inode(|disk_inode| -> Option<Shift> {
            assert!(disk_inode.is_dir());
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut idx = None;
            let mut probe = DirEntry::empty();
            for i in 0..file_count {
                assert_eq!(
                    disk_inode.read_at(
                        DIRENT_SZ * i,
                        probe.as_bytes_mut(),
                        &self.block_device,
                    ),
                    DIRENT_SZ,
                );
                if probe.name() == name {
                    idx = Some(i);
                    break;
                }
            }
            let idx = idx?;
            let mut tail: Vec<DirEntry> = Vec::with_capacity(file_count - idx - 1);
            for i in (idx + 1)..file_count {
                // a fresh entry per slot: `DirEntry` is not `Copy`, so it cannot live
                // outside the loop and be pushed on every iteration
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(
                        DIRENT_SZ * i,
                        dirent.as_bytes_mut(),
                        &self.block_device,
                    ),
                    DIRENT_SZ,
                );
                tail.push(dirent);
            }
            Some(Shift {
                idx,
                file_count,
                tail,
            })
        });
        let Shift {
            idx,
            file_count,
            tail,
        } = match shift {
            Some(shift) => shift,
            None => return false,
        };
        let old_size = (file_count * DIRENT_SZ) as u32;
        let new_size = ((file_count - 1) * DIRENT_SZ) as u32;
        // Shift the tail up and shrink inside one critical section, so no reader can
        // observe the duplicated intermediate state. `decrease_size` returns the blocks
        // (and indirect tables) that leave the directory's range -- with a 32-byte dirent
        // and a 512-byte block that is at most the last data block, and only when the
        // entry count crosses a block boundary.
        self.modify_disk_inode(|disk_inode| {
            for (j, dirent) in tail.iter().enumerate() {
                disk_inode.write_at((idx + j) * DIRENT_SZ, dirent.as_bytes(), &self.block_device);
            }
            // blank the slot that fell past the end of the directory while it is still
            // inside the old size, then shrink
            let empty = DirEntry::empty();
            disk_inode.write_at(
                (file_count - 1) * DIRENT_SZ,
                empty.as_bytes(),
                &self.block_device,
            );
            debug_assert_eq!(old_size, (file_count * DIRENT_SZ) as u32);
            disk_inode.decrease_size(new_size, &self.block_device)
        })
        .into_iter()
        .for_each(|block_id| fs.dealloc_data(block_id));
        block_cache_sync_all();
        true
    }

    /// Release every data block (including indirect blocks) held by this inode and set
    /// its size to zero. Unlike `clear()`, nothing is asserted about the number of
    /// blocks, so it is also safe for an inode whose dirents were removed one by one.
    /// This must only be called once the last hard link is gone.
    pub fn free_blocks(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            for data_block in disk_inode.clear_size(&self.block_device).into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }

    /// Return this inode's id (its index in the inode table). The inverse of
    /// `EasyFileSystem::get_disk_inode_pos`, needed to count hard links.
    pub fn id(&self) -> u32 {
        self.id_with_fs(&self.fs.lock())
    }
    /// Same computation for a caller that already holds the filesystem lock.
    /// The lock is a `spin::Mutex`, which is not reentrant: calling `id()` from
    /// inside `link()` would spin forever instead of panicking.
    fn id_with_fs(&self, fs: &EasyFileSystem) -> u32 {
        let inodes_per_block = (BLOCK_SZ / DISK_INODE_SZ) as u32;
        (self.block_id as u32 - fs.inode_area_start_block()) * inodes_per_block
            + (self.block_offset / DISK_INODE_SZ) as u32
    }
}
