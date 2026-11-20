//! Implementation of [`PageTableEntry`] and [`PageTable`].

use super::{frame_alloc, FrameTracker, PhysAddr, PhysPageNum, StepByOne, VirtAddr, VirtPageNum};
use crate::config::PAGE_SIZE;
use alloc::vec;
use alloc::vec::Vec;
use bitflags::*;

bitflags! {
    /// page table entry flags
    pub struct PTEFlags: u8 {
        /// Valid
        const V = 1 << 0;
        /// Readable
        const R = 1 << 1;
        /// Writable
        const W = 1 << 2;
        /// eXecutable
        const X = 1 << 3;
        /// User
        const U = 1 << 4;
        /// Global
        const G = 1 << 5;
        /// Accessed
        const A = 1 << 6;
        /// Dirty
        const D = 1 << 7;
    }
}

#[derive(Copy, Clone)]
#[repr(C)]
/// page table entry structure
pub struct PageTableEntry {
    /// bits of page table entry
    pub bits: usize,
}

impl PageTableEntry {
    /// Create a new page table entry
    pub fn new(ppn: PhysPageNum, flags: PTEFlags) -> Self {
        PageTableEntry {
            bits: ppn.0 << 10 | flags.bits as usize,
        }
    }
    /// Create an empty page table entry
    pub fn empty() -> Self {
        PageTableEntry { bits: 0 }
    }
    /// Get the physical page number from the page table entry
    pub fn ppn(&self) -> PhysPageNum {
        (self.bits >> 10 & ((1usize << 44) - 1)).into()
    }
    /// Get the flags from the page table entry
    pub fn flags(&self) -> PTEFlags {
        PTEFlags::from_bits(self.bits as u8).unwrap()
    }
    /// The page pointered by page table entry is valid?
    pub fn is_valid(&self) -> bool {
        (self.flags() & PTEFlags::V) != PTEFlags::empty()
    }
    /// The page pointered by page table entry is readable?
    pub fn readable(&self) -> bool {
        (self.flags() & PTEFlags::R) != PTEFlags::empty()
    }
    /// The page pointered by page table entry is writable?
    pub fn writable(&self) -> bool {
        (self.flags() & PTEFlags::W) != PTEFlags::empty()
    }
    /// The page pointered by page table entry is executable?
    pub fn executable(&self) -> bool {
        (self.flags() & PTEFlags::X) != PTEFlags::empty()
    }
}

/// page table structure
pub struct PageTable {
    root_ppn: PhysPageNum,
    frames: Vec<FrameTracker>,
}

/// Assume that it won't oom when creating/mapping.
impl PageTable {
    /// Create a new page table
    pub fn new() -> Self {
        let frame = frame_alloc().unwrap();
        PageTable {
            root_ppn: frame.ppn,
            frames: vec![frame],
        }
    }
    /// Temporarily used to get arguments from user space.
    pub fn from_token(satp: usize) -> Self {
        Self {
            root_ppn: PhysPageNum::from(satp & ((1usize << 44) - 1)),
            frames: Vec::new(),
        }
    }
    /// Find PageTableEntry by VirtPageNum, create a frame for a 4KB page table if not exist
    fn find_pte_create(&mut self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if i == 2 {
                result = Some(pte);
                break;
            }
            if !pte.is_valid() {
                let frame = frame_alloc().unwrap();
                *pte = PageTableEntry::new(frame.ppn, PTEFlags::V);
                self.frames.push(frame);
            }
            ppn = pte.ppn();
        }
        result
    }
    /// Find PageTableEntry by VirtPageNum.
    /// `None` means the SV39 walk stopped early, i.e. no page table for this
    /// virtual page was ever built, so the address is definitely not mapped.
    fn find_pte(&self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[*idx];
            if i == 2 {
                result = Some(pte);
                break;
            }
            if !pte.is_valid() {
                return None;
            }
            ppn = pte.ppn();
        }
        result
    }
    /// set the map between virtual page number and physical page number
    #[allow(unused)]
    pub fn map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        let pte = self.find_pte_create(vpn).unwrap();
        assert!(!pte.is_valid(), "vpn {:?} is mapped before mapping", vpn);
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V);
    }
    /// remove the map between virtual page number and physical page number
    #[allow(unused)]
    pub fn unmap(&mut self, vpn: VirtPageNum) {
        let pte = self.find_pte(vpn).unwrap();
        assert!(pte.is_valid(), "vpn {:?} is invalid before unmapping", vpn);
        *pte = PageTableEntry::empty();
    }
    /// get the page table entry from the virtual page number
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PageTableEntry> {
        self.find_pte(vpn).map(|pte| *pte)
    }
    /// Is there a *valid* leaf mapping for this virtual page? A plain
    /// `translate(..).is_some()` is not enough: after `sys_munmap` the leaf
    /// entry is cleared but its parent page tables are still there, so the walk
    /// succeeds while the entry itself is empty. Treating that as "mapped" (or
    /// "free") would let `sys_mmap` overlap a range that was just torn down and
    /// trip the "already mapped" assertion of `PageTable::map`.
    pub fn is_mapped(&self, vpn: VirtPageNum) -> bool {
        self.translate(vpn).map_or(false, |pte| pte.is_valid())
    }
    /// Translate a virtual address to the physical page it lives in.
    /// `None` means the page is not mapped at all (the SV39 walk stopped early)
    /// or the leaf entry is not valid, so callers can reject a bogus user
    /// pointer instead of faulting on it.
    pub fn translate_va(&self, va: VirtAddr) -> Option<PhysPageNum> {
        self.find_pte(va.floor())
            .filter(|pte| pte.is_valid())
            .map(|pte| pte.ppn())
    }
    /// get the token from the page table
    pub fn token(&self) -> usize {
        8usize << 60 | self.root_ppn.0
    }
}

/// Translate&Copy a ptr[u8] array with LENGTH len to a mutable u8 Vec through page table
pub fn translated_byte_buffer(token: usize, ptr: *const u8, len: usize) -> Vec<&'static mut [u8]> {
    let page_table = PageTable::from_token(token);
    let mut start = ptr as usize;
    let end = start + len;
    let mut v = Vec::new();
    while start < end {
        let start_va = VirtAddr::from(start);
        let mut vpn = start_va.floor();
        let ppn = page_table.translate(vpn).unwrap().ppn();
        vpn.step();
        let mut end_va: VirtAddr = vpn.into();
        end_va = end_va.min(VirtAddr::from(end));
        if end_va.page_offset() == 0 {
            v.push(&mut ppn.get_bytes_array()[start_va.page_offset()..]);
        } else {
            v.push(&mut ppn.get_bytes_array()[start_va.page_offset()..end_va.page_offset()]);
        }
        start = end_va.into();
    }
    v
}

/// Translate a user space pointer and get a mutable reference to the item it
/// points to. Unlike [`translated_byte_buffer`] this works for any `T`, which
/// is what `sys_get_time` needs: the two fields of a `TimeVal` are written
/// separately, so each of them only has to live in one (writable) page.
/// Returns `None` if the item is not addressable, is not mapped as a valid and
/// writable page, or crosses a page boundary.
pub fn try_translated_refmut<T>(token: usize, ptr: *mut T) -> Option<&'static mut T> {
    let size = core::mem::size_of::<T>();
    // A single item never crosses a page as long as it is aligned and small,
    // so only reject genuinely unaligned / oversized accesses here.
    if size == 0 || size > PAGE_SIZE || (ptr as usize) % size != 0 {
        return None;
    }
    if !user_ptr_range_ok(ptr as *const u8, size) {
        return None;
    }
    let page_table = PageTable::from_token(token);
    let va = VirtAddr::from(ptr as usize);
    let first = page_table.translate(va.floor())?;
    let last = page_table.translate(VirtAddr::from(va.0 + size - 1).floor())?;
    if !first.is_valid() || !first.writable() || first.ppn() != last.ppn() {
        return None;
    }
    // `PhysAddr::from(ppn)` is page aligned, so the page offset has to be added
    // back: without it the reference points at byte 0 of the frame instead of
    // the caller's field, which is how `sys_get_time` used to leave the user's
    // `TimeVal` untouched (and `get_time()` reading back 0).
    let pa = PhysAddr::from(first.ppn()).0 + va.page_offset();
    Some(PhysAddr(pa).get_mut::<T>())
}

/// Whether `[ptr, ptr + len)` lies inside the SV39 *user* half of the address
/// space without wrapping around. Used to reject a bogus user pointer before
/// touching the page table, since an unmapped address would otherwise make
/// `translated_*` panic and take the kernel down with it.
/// The two's complement bit pattern of a negative `isize` (e.g. the
/// `isize::MAX`-ish addresses a test may pass) is huge, so it is rejected too.
fn user_ptr_range_ok(ptr: *const u8, len: usize) -> bool {
    const USER_LIMIT: usize = 1usize << 38; // SV39: [0, 2^38) is user space
    let start = ptr as usize;
    if start > isize::MAX as usize {
        return false;
    }
    match start.checked_add(len) {
        Some(end) => start < USER_LIMIT && end <= USER_LIMIT,
        None => false,
    }
}

/// Copy `len` bytes from the user address space into a kernel owned buffer.
/// Returns `None` when the range is not addressable or any page of it is not
/// mapped, so a user program can pass a garbage pointer safely.
pub fn read_user_bytes(token: usize, ptr: *const u8, len: usize) -> Option<Vec<u8>> {
    if len == 0 || !user_ptr_range_ok(ptr, len) {
        return None;
    }
    let page_table = PageTable::from_token(token);
    let mut buf = vec![0u8; len];
    let mut offset = 0;
    while offset < len {
        let va = VirtAddr::from(ptr as usize + offset);
        let pte = page_table.translate(va.floor())?;
        if !pte.is_valid() || !pte.readable() {
            return None;
        }
        let ppn = pte.ppn();
        // stop at the end of this page (or at the end of the request)
        let chunk = PAGE_SIZE - va.page_offset();
        let chunk = chunk.min(len - offset);
        let src = &ppn.get_bytes_array()[va.page_offset()..va.page_offset() + chunk];
        buf[offset..offset + chunk].copy_from_slice(src);
        offset += chunk;
    }
    Some(buf)
}

/// Write `len` bytes from a kernel buffer into the user address space.
/// Same validity rules as [`read_user_bytes`], except that every page of the
/// range also has to be writable.
pub fn write_user_bytes(token: usize, ptr: *mut u8, data: &[u8]) -> bool {
    let len = data.len();
    if len == 0 || !user_ptr_range_ok(ptr, len) {
        return false;
    }
    let page_table = PageTable::from_token(token);
    let mut offset = 0;
    while offset < len {
        let va = VirtAddr::from(ptr as usize + offset);
        let pte = match page_table.translate(va.floor()) {
            Some(pte) => pte,
            None => return false,
        };
        if !pte.is_valid() || !pte.writable() {
            return false;
        }
        let ppn = pte.ppn();
        let chunk = (PAGE_SIZE - va.page_offset()).min(len - offset);
        let dst = &mut ppn.get_bytes_array()[va.page_offset()..va.page_offset() + chunk];
        dst.copy_from_slice(&data[offset..offset + chunk]);
        offset += chunk;
    }
    true
}
