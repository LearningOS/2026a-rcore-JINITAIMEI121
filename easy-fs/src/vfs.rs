//! index node(inode, namely file control block) layer
//!
//! The data struct and functions for the inode layer that service file-related system calls
//!
//! NOTICE: The difference between [`Inode`] and [`DiskInode`]  can be seen from their names: DiskInode in a relatively fixed location within the disk block, while Inode Is a data structure placed in memory that records file inode information.
use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};

/// Inode struct in memory
pub struct Inode {
    /// The block id of the inode
    block_id: usize,
    /// The offset of the inode in the block
    block_offset: usize,
    /// The file system
    fs: Arc<Mutex<EasyFileSystem>>,
    /// The block device
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a new Disk Inode
    ///
    /// We should not acquire efs lock here.
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
    /// read the content of the disk inode on disk with 'f' function
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// modify the content of the disk inode on disk with 'f' function
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// find the disk inode id according to the file with 'name' by search the directory entries in the disk inode with Directory type
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
    /// find the disk inode of the file with 'name'
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
    /// increase the size of file( also known as 'disk inode')
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
    /// create a file with 'name' in the root directory
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        let mut fs = self.fs.lock();
        let op = |root_inode: &mut DiskInode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(name, root_inode)
        };
        if self.modify_disk_inode(op).is_some() {
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
    /// create a directory with 'name' in the root directory
    ///
    /// list the file names in the root directory
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

    /// Return the number of directory entries (i.e. hard links) of this inode.
    pub fn nlink(&self) -> u32 {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.nlink as u32)
    }

    /// Is this inode a directory?
    pub fn is_dir(&self) -> bool {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.is_dir())
    }

    /// The raw id of this inode inside the inode area.
    pub fn id(&self) -> usize {
        let fs = self.fs.lock();
        fs.inode_id_of(self.block_id, self.block_offset)
    }

    /// Build an [`Inode`] handle for a raw inode id living in `fs`.
    pub fn new_with_id(
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
        inode_id: u32,
    ) -> Arc<Self> {
        let (block_id, block_offset) = fs.lock().get_disk_inode_pos(inode_id);
        Arc::new(Self::new(block_id, block_offset, fs, block_device))
    }

    /// Return the inode id of the entry called `name` in this directory.
    pub fn lookup_inode_id(&self, name: &str) -> Option<u32> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| self.find_inode_id(name, disk_inode))
    }

    /// Add a new directory entry `name` in this directory pointing to an
    /// existing inode whose id is `inode_id`. This is the backend of the
    /// `linkat` syscall.
    pub fn link(&self, name: &str, inode_id: u32) -> Option<u32> {
        let mut fs = self.fs.lock();
        if self.read_disk_inode(|disk_inode| self.find_inode_id(name, disk_inode)).is_some() {
            return None;
        }
        // bump the nlink counter of the target inode
        let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
        get_block_cache(block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(block_offset, |disk_inode: &mut DiskInode| {
                disk_inode.nlink += 1;
            });
        self.modify_disk_inode(|root_inode| {
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) as u32 * DIRENT_SZ as u32;
            self.increase_size(new_size, root_inode, &mut fs);
            let dirent = DirEntry::new(name, inode_id);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });
        block_cache_sync_all();
        Some(inode_id)
    }

    /// Remove the directory entry `name` from this directory and decrease the
    /// nlink counter of the inode it points to. When the counter reaches zero
    /// the inode (and all its data blocks) are released. Backend of `unlinkat`.
    pub fn unlink(&self, name: &str) -> bool {
        let mut fs = self.fs.lock();
        let inode_id =
            match self.read_disk_inode(|disk_inode| self.find_inode_id(name, disk_inode)) {
                Some(id) => id,
                None => return false,
            };
        // drop the directory entry by shifting the following ones leftwards
        let deleted = self.modify_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            if file_count == 0 {
                return false;
            }
            // collect every entry except the one being removed
            let mut entries: Vec<DirEntry> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                disk_inode.read_at(
                    i * DIRENT_SZ,
                    dirent.as_bytes_mut(),
                    &self.block_device,
                );
                if dirent.name() != name {
                    entries.push(dirent);
                }
            }
            if entries.len() == file_count {
                return false;
            }
            let new_size = entries.len() as u32 * DIRENT_SZ as u32;
            // easy-fs can only grow an inode, so when the tail data block of the
            // directory disappears we rebuild the whole directory. The root
            // directory needs one block per 16 entries, so this is cheap.
            let old_total_blocks = DiskInode::total_blocks(disk_inode.size);
            let new_total_blocks = DiskInode::total_blocks(new_size);
            if new_total_blocks < old_total_blocks {
                let old_blocks = disk_inode.clear_size(&self.block_device);
                for block in old_blocks.into_iter() {
                    fs.dealloc_data(block);
                }
                let mut fresh: Vec<u32> = Vec::new();
                for _ in 0..new_total_blocks {
                    fresh.push(fs.alloc_data());
                }
                disk_inode.increase_size(new_size, fresh, &self.block_device);
            } else {
                // keep the already allocated blocks, just shorten the file
                disk_inode.size = new_size;
                for (i, dirent) in entries.iter().enumerate() {
                    disk_inode.write_at(i * DIRENT_SZ, dirent.as_bytes(), &self.block_device);
                }
                return true;
            }
            for (i, dirent) in entries.iter().enumerate() {
                disk_inode.write_at(i * DIRENT_SZ, dirent.as_bytes(), &self.block_device);
            }
            true
        });
        if !deleted {
            return false;
        }
        // decrease the nlink of the target inode, release it if it is the last one
        let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
        let should_free = get_block_cache(block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(block_offset, |disk_inode: &mut DiskInode| {
                if disk_inode.nlink > 0 {
                    disk_inode.nlink -= 1;
                }
                disk_inode.nlink == 0
            });
        if should_free {
            get_block_cache(block_id as usize, Arc::clone(&self.block_device)).lock().modify(
                block_offset,
                |disk_inode: &mut DiskInode| {
                    let size = disk_inode.size;
                    let blocks = disk_inode.clear_size(&self.block_device);
                    assert!(blocks.len() == DiskInode::total_blocks(size) as usize);
                    for data_block in blocks.into_iter() {
                        fs.dealloc_data(data_block);
                    }
                },
            );
        }
        block_cache_sync_all();
        true
    }
    /// Read the content in offset position of the file into 'buf'
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write the content in 'buf' into offset position of the file
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Set the file(disk inode) length to zero, delloc all data blocks of the file.
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
}
