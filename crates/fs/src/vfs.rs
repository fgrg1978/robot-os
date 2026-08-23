//! VFS — Virtual Filesystem
//!
//! Direct port of kernel/fs/fs.c + kernel/include/fs.h.
//! Implements a simple in-memory ramfs with VFS abstraction.
//! Inode data and directory entries are heap-allocated; the inode pool is a
//! fixed static array protected by a global spinlock.

use alloc::alloc::{alloc, dealloc, Layout};
use core::ptr;
use robot_os_sync::SpinLock;
pub use robot_os_limits::MAX_FDS_PER_PROC as MAX_FDS;

// ─── Constants ────────────────────────────────────────────────────────────────

pub const MAX_FILES:    usize = 128;
pub const MAX_FILENAME: usize = 64;
pub const MAX_PATH:     usize = 256;
pub const MAX_MOUNTS:   usize = 4;

pub const INODE_FILE:   u8 = 1;
pub const INODE_DIR:    u8 = 2;
pub const INODE_DEVICE: u8 = 3;

pub const PERM_READ:  u32 = 0x4;
pub const PERM_WRITE: u32 = 0x2;
pub const PERM_EXEC:  u32 = 0x1;

pub const O_RDONLY: u32 = 0x0;
pub const O_WRONLY: u32 = 0x1;
pub const O_RDWR:   u32 = 0x2;
pub const O_CREAT:  u32 = 0x40;
pub const O_TRUNC:  u32 = 0x200;
pub const O_APPEND: u32 = 0x400;

pub const SEEK_SET: i32 = 0;
pub const SEEK_CUR: i32 = 1;
pub const SEEK_END: i32 = 2;

pub const FS_TYPE_RAMFS: u32 = 0;
pub const FS_TYPE_FAT32: u32 = 1;

/// Null sentinel for inode indices (equivalent to C's NULL pointer).
pub const NO_IDX: u32 = u32::MAX;

// ─── Inode ────────────────────────────────────────────────────────────────────

/// In-kernel inode — direct port of `inode_t` in kernel/include/fs.h.
///
/// Uses raw pointers so it can live in a `static` array and be `Copy`.
/// All access is protected by `FS` spinlock.
#[derive(Copy, Clone)]
pub struct Inode {
    pub ino:         u32,
    pub itype:       u8,       // INODE_FILE / INODE_DIR / INODE_DEVICE
    pub size:        u32,
    pub permissions: u32,
    // File data (heap-allocated; INODE_FILE only)
    pub data:        *mut u8,
    pub capacity:    u32,
    // Directory entries (heap-allocated; INODE_DIR only)
    pub entries:     *mut DentryEntry,
    pub entry_count: u32,
    // Device callbacks (INODE_DEVICE only)
    pub dev_read:    Option<unsafe fn(*mut u8, usize) -> i32>,
    pub dev_write:   Option<unsafe fn(*const u8, usize) -> i32>,
    // Metadata
    pub ref_count:   u32,
    pub link_count:  u32,
    // FAT32 backing (Phase 8): flush inode data to disk on close when dirty.
    pub fat32_backed: bool,
    pub fat32_dirty:  bool,
    pub fat32_name:   [u8; 11],  // 8.3 name used for flush
}

const ZEROED_INODE: Inode = Inode {
    ino: 0, itype: 0, size: 0, permissions: 0,
    data: ptr::null_mut(), capacity: 0,
    entries: ptr::null_mut(), entry_count: 0,
    dev_read: None, dev_write: None,
    ref_count: 0, link_count: 0,
    fat32_backed: false, fat32_dirty: false, fat32_name: [0u8; 11],
};

// Safety: all Inode access is serialised by the FS spinlock.
unsafe impl Send for Inode {}

// ─── DentryEntry ──────────────────────────────────────────────────────────────

/// Directory entry — port of `dentry_t`, but stores a pool index instead of a
/// raw pointer (avoids dangling-pointer UB after inode pool compaction).
#[derive(Copy, Clone)]
pub struct DentryEntry {
    pub name:      [u8; MAX_FILENAME],
    pub inode_idx: u32,   // Index into FsGlobal::inodes
}

const ZEROED_DENTRY: DentryEntry = DentryEntry {
    name: [0u8; MAX_FILENAME],
    inode_idx: NO_IDX,
};

// ─── FileDesc ─────────────────────────────────────────────────────────────────

/// Open file descriptor — port of `file_desc_t`.
#[derive(Copy, Clone)]
pub struct FileDesc {
    pub inode_idx: u32,   // NO_IDX = no inode (or pipe — future)
    pub offset:    u32,
    pub flags:     u32,
    pub in_use:    bool,
}

const ZEROED_FD: FileDesc = FileDesc {
    inode_idx: NO_IDX, offset: 0, flags: 0, in_use: false,
};

// ─── FdTable ──────────────────────────────────────────────────────────────────

/// Per-task file descriptor table — port of `fd_table_t`.
#[derive(Copy, Clone)]
pub struct FdTable {
    pub fds: [FileDesc; MAX_FDS],
}

impl FdTable {
    pub const fn new() -> Self {
        FdTable { fds: [ZEROED_FD; MAX_FDS] }
    }
}

// ─── MountPoint ───────────────────────────────────────────────────────────────

#[derive(Copy, Clone)]
pub struct MountPoint {
    pub path:    [u8; 64],
    pub fs_type: u32,
    pub active:  bool,
    pub fs_idx:  u32,   // For FAT32: index into fat32 state table
}

const ZEROED_MOUNT: MountPoint = MountPoint {
    path: [0u8; 64], fs_type: 0, active: false, fs_idx: 0,
};

// ─── Global FS state ──────────────────────────────────────────────────────────

struct FsGlobal {
    inodes:      [Inode; MAX_FILES],
    root_idx:    u32,
    next_ino:    u32,
    mounts:      [MountPoint; MAX_MOUNTS],
    mount_count: usize,
}

const ZEROED_FS: FsGlobal = FsGlobal {
    inodes:      [ZEROED_INODE; MAX_FILES],
    root_idx:    NO_IDX,
    next_ino:    1,
    mounts:      [ZEROED_MOUNT; MAX_MOUNTS],
    mount_count: 0,
};

// Safety: all access is serialised by the SpinLock.
unsafe impl Send for FsGlobal {}

static FS: SpinLock<FsGlobal> = SpinLock::new(ZEROED_FS);

/// Non-blocking check for whether the global `FS` lock is currently free.
///
/// Intended for callers that must never block (e.g. the panic handler),
/// which cannot afford to spin on `FS` if some hart is holding it — for
/// instance because that hart is itself stuck inside a panic, or panicked
/// while the lock was held and `panic = abort` means it will never be
/// released.
///
/// This is inherently racy: another hart may acquire `FS` immediately
/// after this returns `true`, and `vfs_open`/`vfs_write`/`vfs_close` each
/// take and release `FS` multiple times internally, so a caller can still
/// end up spinning on a later acquisition even after observing `true`
/// here. This function only rules out the common, most dangerous case
/// where the lock is already held at the time of the check — it is not a
/// full non-blocking guarantee for the VFS call that follows.
pub fn vfs_fs_lock_available() -> bool {
    FS.try_lock().is_some()
}

// ─── String / path helpers ────────────────────────────────────────────────────

/// Compare a byte slice with a fixed-size null-terminated name array.
fn name_eq(a: &[u8], b: &[u8; MAX_FILENAME]) -> bool {
    let b_len = b.iter().position(|&c| c == 0).unwrap_or(MAX_FILENAME);
    a.len() == b_len && &a[..] == &b[..b_len]
}

/// Copy a byte slice into a null-terminated fixed-size name array.
fn name_copy(dst: &mut [u8; MAX_FILENAME], src: &[u8]) {
    let n = src.len().min(MAX_FILENAME - 1);
    dst[..n].copy_from_slice(&src[..n]);
    dst[n] = 0;
}

/// Trim a byte slice at the first NUL byte.
fn trim_nul(s: &[u8]) -> &[u8] {
    if let Some(n) = s.iter().position(|&c| c == 0) { &s[..n] } else { s }
}

/// Convert a C-string pointer to a Rust byte slice (without NUL terminator).
/// # Safety
/// `ptr` must be a valid, non-null, NUL-terminated byte string.
pub unsafe fn cstr_to_bytes<'a>(ptr: *const u8) -> &'a [u8] {
    if ptr.is_null() { return &[]; }
    let mut len = 0usize;
    while *ptr.add(len) != 0 { len += 1; }
    core::slice::from_raw_parts(ptr, len)
}

/// Return true if `path` starts with `prefix` followed by '/' or NUL.
fn path_starts_with(path: &[u8], prefix: &[u8]) -> bool {
    if path.len() < prefix.len() || &path[..prefix.len()] != prefix {
        return false;
    }
    matches!(path.get(prefix.len()), None | Some(&b'/') | Some(&0))
}

/// Iterate path components split by '/'.
struct PathComponents<'a> {
    rest: &'a [u8],
}

impl<'a> PathComponents<'a> {
    fn new(path: &'a [u8]) -> Self {
        // Skip leading '/' and trim NUL
        let rest = path.strip_prefix(b"/").unwrap_or(path);
        PathComponents { rest: trim_nul(rest) }
    }
}

impl<'a> Iterator for PathComponents<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        while self.rest.first() == Some(&b'/') {
            self.rest = &self.rest[1..];
        }
        if self.rest.is_empty() { return None; }

        let end = self.rest.iter()
            .position(|&c| c == b'/')
            .unwrap_or(self.rest.len());
        let component = &self.rest[..end];
        self.rest = if end < self.rest.len() { &self.rest[end + 1..] } else { &[] };
        Some(component)
    }
}

// ─── Allocator helpers ────────────────────────────────────────────────────────

/// Allocate `size` bytes with alignment 1.  Returns null on failure or size==0.
unsafe fn alloc_raw(size: usize) -> *mut u8 {
    if size == 0 { return ptr::null_mut(); }
    alloc(Layout::from_size_align(size, 1).unwrap())
}

/// Deallocate a byte buffer allocated with `alloc_raw`.
unsafe fn dealloc_raw(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        dealloc(ptr, Layout::from_size_align(size, 1).unwrap());
    }
}

/// Allocate an array of `count` `DentryEntry` values.
unsafe fn alloc_dentries(count: usize) -> *mut DentryEntry {
    if count == 0 { return ptr::null_mut(); }
    let layout = Layout::array::<DentryEntry>(count).unwrap();
    alloc(layout) as *mut DentryEntry
}

/// Deallocate a `DentryEntry` array allocated with `alloc_dentries`.
unsafe fn dealloc_dentries(ptr: *mut DentryEntry, count: usize) {
    if !ptr.is_null() && count > 0 {
        dealloc(ptr as *mut u8, Layout::array::<DentryEntry>(count).unwrap());
    }
}

// ─── Inode operations — port of `inode_alloc / inode_free / inode_resize` ─────

/// Allocate a new inode from the pool.
/// Returns index into the pool, or `NO_IDX` on failure.
pub fn inode_alloc(itype: u8, permissions: u32) -> u32 {
    let mut fs = FS.lock();
    for i in 0..MAX_FILES {
        if fs.inodes[i].ino == 0 {
            let ino = fs.next_ino;
            fs.next_ino += 1;
            fs.inodes[i] = ZEROED_INODE;
            fs.inodes[i].ino         = ino;
            fs.inodes[i].itype       = itype;
            fs.inodes[i].permissions = permissions;
            fs.inodes[i].link_count  = 1;
            return i as u32;
        }
    }
    NO_IDX
}

/// Free an inode and release its heap memory.
pub fn inode_free(idx: u32) {
    if idx == NO_IDX || idx as usize >= MAX_FILES { return; }

    // Atomically zero the inode slot and capture the pointers to free.
    let (data, cap, entries, entry_count) = {
        let mut fs = FS.lock();
        let n = &mut fs.inodes[idx as usize];
        if n.ino == 0 { return; }   // Already free
        let ptrs = (n.data, n.capacity, n.entries, n.entry_count);
        *n = ZEROED_INODE;
        ptrs
    };

    // Free heap memory after releasing the lock.
    unsafe {
        dealloc_raw(data, cap as usize);
        dealloc_dentries(entries, entry_count as usize);
    }
}

/// Resize a file inode's data buffer.
/// Only grows the allocation; shrinking is only done when `new_size == 0`.
/// Port of `inode_resize()` in fs.c.
pub fn inode_resize(idx: u32, new_size: u32) -> Result<(), ()> {
    if idx == NO_IDX || idx as usize >= MAX_FILES { return Err(()); }

    let (old_data, old_cap, old_size) = {
        let fs = FS.lock();
        let n = &fs.inodes[idx as usize];
        (n.data, n.capacity, n.size)
    };

    if new_size == 0 {
        unsafe { dealloc_raw(old_data, old_cap as usize); }
        let mut fs = FS.lock();
        let n = &mut fs.inodes[idx as usize];
        n.data = ptr::null_mut();
        n.size = 0;
        n.capacity = 0;
        return Ok(());
    }

    if new_size <= old_cap {
        // Buffer already large enough — just update the logical size.
        FS.lock().inodes[idx as usize].size = new_size;
        return Ok(());
    }

    // Need to grow: allocate a new buffer.
    let new_data = unsafe { alloc_raw(new_size as usize) };
    if new_data.is_null() { return Err(()); }

    unsafe {
        // Copy existing content.
        if !old_data.is_null() && old_size > 0 {
            ptr::copy_nonoverlapping(old_data, new_data, old_size as usize);
        }
        // Zero the newly added region.
        if new_size > old_size {
            ptr::write_bytes(
                new_data.add(old_size as usize),
                0,
                (new_size - old_size) as usize,
            );
        }
        dealloc_raw(old_data, old_cap as usize);
    }

    let mut fs = FS.lock();
    let n = &mut fs.inodes[idx as usize];
    n.data     = new_data;
    n.size     = new_size;
    n.capacity = new_size;
    Ok(())
}

// ─── Directory operations — port of `dir_add/lookup/remove_entry` in fs.c ─────

/// Add a directory entry.  Mirrors `dir_add_entry()` in fs.c.
pub fn dir_add_entry(dir_idx: u32, name: &[u8], inode_idx: u32) -> Result<(), ()> {
    if dir_idx == NO_IDX || inode_idx == NO_IDX { return Err(()); }
    if name.is_empty() || name.len() >= MAX_FILENAME { return Err(()); }

    // Validate and snapshot the current entry list under the lock.
    let (old_entries, old_count) = {
        let fs = FS.lock();
        let dir = &fs.inodes[dir_idx as usize];
        if dir.itype != INODE_DIR { return Err(()); }
        // Duplicate check
        for i in 0..dir.entry_count as usize {
            let ent = unsafe { &*dir.entries.add(i) };
            if name_eq(name, &ent.name) { return Err(()); }
        }
        (dir.entries, dir.entry_count)
    };

    let new_count = old_count + 1;

    // Allocate the new (larger) entry array outside the lock.
    let new_entries = unsafe { alloc_dentries(new_count as usize) };
    if new_entries.is_null() { return Err(()); }

    unsafe {
        // Copy existing entries.
        if !old_entries.is_null() && old_count > 0 {
            ptr::copy_nonoverlapping(old_entries, new_entries, old_count as usize);
        }
        // Append the new entry.
        let slot = &mut *new_entries.add(old_count as usize);
        *slot = ZEROED_DENTRY;
        name_copy(&mut slot.name, name);
        slot.inode_idx = inode_idx;
    }

    // Update the directory inode and bump link count under the lock.
    let old_to_free = {
        let mut fs = FS.lock();
        let dir = &mut fs.inodes[dir_idx as usize];
        // Guard against a concurrent modification (should not happen in Phase 6).
        if dir.entry_count != old_count {
            unsafe { dealloc_dentries(new_entries, new_count as usize); }
            return Err(());
        }
        let old = dir.entries;
        dir.entries     = new_entries;
        dir.entry_count = new_count;
        if (inode_idx as usize) < MAX_FILES {
            fs.inodes[inode_idx as usize].link_count += 1;
        }
        old
    };

    unsafe { dealloc_dentries(old_to_free, old_count as usize); }
    Ok(())
}

/// Find a directory entry by name.  Returns inode index or `NO_IDX`.
pub fn dir_lookup(dir_idx: u32, name: &[u8]) -> u32 {
    if dir_idx == NO_IDX || dir_idx as usize >= MAX_FILES { return NO_IDX; }

    let fs = FS.lock();
    let dir = &fs.inodes[dir_idx as usize];
    if dir.itype != INODE_DIR || dir.entries.is_null() { return NO_IDX; }

    for i in 0..dir.entry_count as usize {
        let ent = unsafe { &*dir.entries.add(i) };
        if name_eq(name, &ent.name) {
            return ent.inode_idx;
        }
    }
    NO_IDX
}

/// Remove a directory entry.  Port of `dir_remove_entry()` in fs.c.
pub fn dir_remove_entry(dir_idx: u32, name: &[u8]) -> Result<(), ()> {
    if dir_idx == NO_IDX || dir_idx as usize >= MAX_FILES { return Err(()); }

    // Find the entry and snapshot list info.
    let (target_idx, old_entries, old_count) = {
        let fs = FS.lock();
        let dir = &fs.inodes[dir_idx as usize];
        if dir.itype != INODE_DIR { return Err(()); }
        let mut found = NO_IDX;
        for i in 0..dir.entry_count as usize {
            let ent = unsafe { &*dir.entries.add(i) };
            if name_eq(name, &ent.name) { found = ent.inode_idx; break; }
        }
        if found == NO_IDX { return Err(()); }
        (found, dir.entries, dir.entry_count)
    };

    let new_count = old_count - 1;
    let new_entries = if new_count > 0 {
        let p = unsafe { alloc_dentries(new_count as usize) };
        if p.is_null() { return Err(()); }
        // Copy all entries except the removed one.
        unsafe {
            let mut out = 0usize;
            for i in 0..old_count as usize {
                let ent = &*old_entries.add(i);
                if !name_eq(name, &ent.name) {
                    *p.add(out) = *ent;
                    out += 1;
                }
            }
        }
        p
    } else {
        ptr::null_mut()
    };

    // Swap entries and update link count.
    let old_to_free = {
        let mut fs = FS.lock();
        let dir = &mut fs.inodes[dir_idx as usize];
        let old = dir.entries;
        dir.entries     = new_entries;
        dir.entry_count = new_count;
        if (target_idx as usize) < MAX_FILES {
            let t = &mut fs.inodes[target_idx as usize];
            if t.link_count > 0 { t.link_count -= 1; }
        }
        old
    };

    unsafe { dealloc_dentries(old_to_free, old_count as usize); }
    Ok(())
}

// ─── Path resolution — port of `path_lookup / path_parent` in fs.c ───────────

/// Resolve an absolute path to an inode index.
/// Returns `NO_IDX` if the path does not exist.
pub fn path_lookup(path: &[u8]) -> u32 {
    let path = trim_nul(path);
    if path.is_empty() || path[0] != b'/' { return NO_IDX; }

    // Root.
    if path == b"/" {
        return FS.lock().root_idx;
    }

    // Check mount points first.
    let (mount_count, mounts) = {
        let fs = FS.lock();
        let mc = fs.mount_count;
        let m = fs.mounts;
        (mc, m)
    };

    for i in 0..mount_count {
        let mp = &mounts[i];
        if !mp.active { continue; }
        let mp_path = trim_nul(&mp.path);
        if path_starts_with(path, mp_path) {
            let sub = &path[mp_path.len()..];
            if mp.fs_type == FS_TYPE_FAT32 {
                // FAT32 files are opened via try_fat32_open() in vfs_open().
                // path_lookup() does not create proxy inodes; return NO_IDX.
                let _ = (mp.fs_idx, sub); // suppress unused warnings
                return NO_IDX;
            }
        }
    }

    // Walk ramfs.
    let root_idx = FS.lock().root_idx;
    let mut current = root_idx;
    for component in PathComponents::new(path) {
        current = dir_lookup(current, component);
        if current == NO_IDX { return NO_IDX; }
    }
    current
}

/// Register a filesystem mount point.
pub fn vfs_mount(path: &[u8], fs_type: u32) -> Result<(), ()> {
    let mut fs = FS.lock();
    if fs.mount_count >= MAX_MOUNTS { return Err(()); }
    let idx = fs.mount_count;   // snapshot before mutable borrow
    let mp = &mut fs.mounts[idx];
    let n = path.len().min(63);
    mp.path[..n].copy_from_slice(&path[..n]);
    mp.path[n] = 0;
    mp.fs_type = fs_type;
    mp.active  = true;
    mp.fs_idx  = 0;
    fs.mount_count += 1;
    Ok(())
}

/// Get a single directory entry by index.
///
/// Returns `Some((name_bytes, size, is_dir))` for the entry at `index`,
/// or `None` if the index is out of range or the inode is not a directory.
pub fn dir_entry_at(dir_idx: u32, index: u32) -> Option<([u8; MAX_FILENAME], u32, bool)> {
    if dir_idx == NO_IDX || dir_idx as usize >= MAX_FILES { return None; }
    let fs = FS.lock();
    let dir = &fs.inodes[dir_idx as usize];
    if dir.itype != INODE_DIR || dir.entries.is_null() { return None; }
    if index >= dir.entry_count { return None; }
    let ent = unsafe { &*dir.entries.add(index as usize) };
    let (size, is_dir) = if (ent.inode_idx as usize) < MAX_FILES {
        let child = &fs.inodes[ent.inode_idx as usize];
        (child.size, child.itype == INODE_DIR)
    } else {
        (0, false)
    };
    Some((ent.name, size, is_dir))
}

/// Iterate all entries in a directory inode, calling `cb(name, inode_type)`.
///
/// # Note
/// Holds the FS lock during iteration. The callback must not acquire
/// the FS lock itself to avoid deadlock.
pub fn dir_list(dir_idx: u32, mut cb: impl FnMut(&[u8], u8)) {
    if dir_idx == NO_IDX || dir_idx as usize >= MAX_FILES { return; }
    let fs = FS.lock();
    let dir = &fs.inodes[dir_idx as usize];
    if dir.itype != INODE_DIR || dir.entries.is_null() { return; }
    for i in 0..dir.entry_count as usize {
        let ent   = unsafe { &*dir.entries.add(i) };
        let itype = if (ent.inode_idx as usize) < MAX_FILES {
            fs.inodes[ent.inode_idx as usize].itype
        } else { 0 };
        let name_len = ent.name.iter().position(|&b| b == 0).unwrap_or(MAX_FILENAME);
        cb(&ent.name[..name_len], itype);
    }
}

/// Convert a bare filename (no slashes) to FAT32 8.3 uppercase format.
/// Returns None if the name does not fit 8.3 layout.
fn path_to_83(name: &[u8]) -> Option<[u8; 11]> {
    if name.is_empty() { return None; }
    let (base, ext) = match name.iter().position(|&b| b == b'.') {
        Some(i) => (&name[..i], &name[i + 1..]),
        None    => (name, &[][..]),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 { return None; }
    let mut result = [b' '; 11];
    for (i, &b) in base.iter().enumerate() { result[i]     = b.to_ascii_uppercase(); }
    for (i, &b) in ext.iter().enumerate()  { result[8 + i] = b.to_ascii_uppercase(); }
    Some(result)
}

/// Try to open a file from a FAT32-mounted volume as a proxy inode.
///
/// Creates a temporary INODE_FILE with the file content loaded from disk.
/// link_count is set to 0 so the inode is freed automatically when closed.
fn try_fat32_open(path: &[u8]) -> u32 {
    let path = trim_nul(path);

    // Snapshot mount table (MountPoint is Copy).
    let (mount_count, mounts) = {
        let fs = FS.lock();
        (fs.mount_count, fs.mounts)
    };

    // Find a FAT32 mount whose path is a prefix of `path`.
    let mut sub: Option<&[u8]> = None;
    for i in 0..mount_count {
        let mp = &mounts[i];
        if !mp.active || mp.fs_type != FS_TYPE_FAT32 { continue; }
        let mp_path = trim_nul(&mp.path);
        if path_starts_with(path, mp_path) {
            let after = &path[mp_path.len()..];
            sub = Some(after.strip_prefix(b"/").unwrap_or(after));
            break;
        }
    }
    let sub = match sub {
        Some(s) => s,
        None    => return NO_IDX,
    };

    // Phase 6: root-directory files only (no subdirectories yet).
    if sub.is_empty() || sub.contains(&b'/') { return NO_IDX; }

    let name83 = match path_to_83(sub) {
        Some(n) => n,
        None    => return NO_IDX,
    };

    let (cluster, file_size) = match crate::fat32::fat32_lookup_root(&name83) {
        Ok(r)   => r,
        Err(()) => return NO_IDX,
    };

    // Allocate proxy inode.
    let inode_idx = inode_alloc(INODE_FILE, PERM_READ);
    if inode_idx == NO_IDX { return NO_IDX; }

    if file_size > 0 {
        if inode_resize(inode_idx, file_size).is_err() {
            inode_free(inode_idx);
            return NO_IDX;
        }
        let data_ptr = FS.lock().inodes[inode_idx as usize].data;
        // Safety: inode_resize guarantees data != null and capacity >= file_size.
        let buf = unsafe { core::slice::from_raw_parts_mut(data_ptr, file_size as usize) };
        crate::fat32::fat32_read_chain(cluster, buf);
    }

    // link_count = 0: freed when last FD releases it (rc==0 && lc==0).
    // Tag with FAT32 backing so writes are flushed to disk on close.
    {
        let mut fs = FS.lock();
        let inode = &mut fs.inodes[inode_idx as usize];
        inode.link_count   = 0;
        inode.fat32_backed = true;
        inode.fat32_name   = name83;
    }
    inode_idx
}

/// Try to create a new empty FAT32-backed proxy inode for a file under a FAT32
/// mount.  Used by `vfs_open` when `O_CREAT` is set.
///
/// Returns `Some(inode_idx)` if the path falls under a FAT32 mount and the
/// filename is a valid 8.3 name; `None` otherwise.
fn try_fat32_create(path: &[u8]) -> Option<u32> {
    let path = trim_nul(path);

    let (mount_count, mounts) = {
        let fs = FS.lock();
        (fs.mount_count, fs.mounts)
    };

    let mut sub: Option<&[u8]> = None;
    for i in 0..mount_count {
        let mp = &mounts[i];
        if !mp.active || mp.fs_type != FS_TYPE_FAT32 { continue; }
        let mp_path = trim_nul(&mp.path);
        if path_starts_with(path, mp_path) {
            let after = &path[mp_path.len()..];
            sub = Some(after.strip_prefix(b"/").unwrap_or(after));
            break;
        }
    }
    let sub = sub?;
    if sub.is_empty() || sub.contains(&b'/') { return None; }

    let name83 = path_to_83(sub)?;

    let inode_idx = inode_alloc(INODE_FILE, PERM_READ | PERM_WRITE);
    if inode_idx == NO_IDX { return None; }

    {
        let mut fs = FS.lock();
        let inode = &mut fs.inodes[inode_idx as usize];
        inode.link_count   = 0;   // auto-freed when last FD closes
        inode.fat32_backed = true;
        inode.fat32_name   = name83;
    }
    Some(inode_idx)
}

/// Return the parent directory index and filename for an absolute path.
/// Equivalent to `path_parent()` in fs.c.
pub fn path_parent(path: &[u8]) -> (u32, &[u8]) {
    let path = trim_nul(path);
    if path.is_empty() || path[0] != b'/' { return (NO_IDX, &[]); }

    // Find the last '/'.
    let last_slash = match path.iter().rposition(|&c| c == b'/') {
        Some(i) => i,
        None    => return (NO_IDX, &[]),
    };
    let filename = &path[last_slash + 1..];
    if filename.is_empty() { return (NO_IDX, &[]); }

    let parent_idx = if last_slash == 0 {
        FS.lock().root_idx          // "/filename" → parent is "/"
    } else {
        path_lookup(&path[..last_slash])
    };
    (parent_idx, filename)
}

// ─── FD table operations — port of `fd_table_init / fd_alloc / fd_free / fd_get` ──

/// Initialize an FD table, opening stdin/stdout/stderr.
/// Port of `fd_table_init()` in fs.c.
pub fn fd_table_init(table: &mut FdTable) {
    *table = FdTable::new();

    let stdin_idx  = path_lookup(b"/dev/stdin");
    let stdout_idx = path_lookup(b"/dev/stdout");
    let stderr_idx = path_lookup(b"/dev/stderr");

    macro_rules! open_std {
        ($idx:expr, $fd:expr, $flags:expr) => {
            if $idx != NO_IDX {
                table.fds[$fd] = FileDesc {
                    inode_idx: $idx, offset: 0, flags: $flags, in_use: true,
                };
                FS.lock().inodes[$idx as usize].ref_count += 1;
            }
        };
    }
    open_std!(stdin_idx,  0, O_RDONLY);
    open_std!(stdout_idx, 1, O_WRONLY);
    open_std!(stderr_idx, 2, O_WRONLY);
}

/// Allocate a file descriptor in a table.  Skips FDs 0-2 (std streams).
/// Returns the fd number on success, or -1 on failure.
pub fn fd_alloc(table: &mut FdTable, inode_idx: u32, flags: u32) -> i32 {
    for fd in 3..MAX_FDS {
        if !table.fds[fd].in_use {
            table.fds[fd] = FileDesc { inode_idx, offset: 0, flags, in_use: true };
            if inode_idx != NO_IDX && (inode_idx as usize) < MAX_FILES {
                FS.lock().inodes[inode_idx as usize].ref_count += 1;
            }
            return fd as i32;
        }
    }
    -1
}

/// Release a file descriptor.
pub fn fd_free(table: &mut FdTable, fd: i32) {
    if fd < 0 || fd as usize >= MAX_FDS || !table.fds[fd as usize].in_use { return; }

    let inode_idx = table.fds[fd as usize].inode_idx;
    table.fds[fd as usize].in_use = false;

    if inode_idx != NO_IDX && (inode_idx as usize) < MAX_FILES {
        let (rc, lc) = {
            let mut fs = FS.lock();
            let n = &mut fs.inodes[inode_idx as usize];
            if n.ref_count > 0 { n.ref_count -= 1; }
            (n.ref_count, n.link_count)
        };
        if rc == 0 && lc == 0 {
            inode_free(inode_idx);
        }
    }
}

/// Get a reference to a file descriptor entry.
pub fn fd_get(table: &FdTable, fd: i32) -> Option<&FileDesc> {
    if fd < 0 || fd as usize >= MAX_FDS || !table.fds[fd as usize].in_use {
        return None;
    }
    Some(&table.fds[fd as usize])
}

/// Duplicate a file descriptor.  Returns the new fd (lowest available), or -1.
pub fn fd_dup(table: &mut FdTable, old_fd: i32) -> i32 {
    if old_fd < 0 || old_fd as usize >= MAX_FDS || !table.fds[old_fd as usize].in_use {
        return -1;
    }
    let entry = table.fds[old_fd as usize];
    // Find the lowest free fd (starting from 0)
    for fd in 0..MAX_FDS {
        if !table.fds[fd].in_use {
            table.fds[fd] = FileDesc {
                inode_idx: entry.inode_idx, offset: entry.offset,
                flags: entry.flags, in_use: true,
            };
            if entry.inode_idx != NO_IDX && (entry.inode_idx as usize) < MAX_FILES {
                FS.lock().inodes[entry.inode_idx as usize].ref_count += 1;
            }
            return fd as i32;
        }
    }
    -1
}

/// Duplicate a file descriptor to a specific fd number.
/// Closes new_fd if already open.  Returns new_fd on success, -1 on error.
pub fn fd_dup2(table: &mut FdTable, old_fd: i32, new_fd: i32) -> i32 {
    if old_fd < 0 || old_fd as usize >= MAX_FDS || !table.fds[old_fd as usize].in_use {
        return -1;
    }
    if new_fd < 0 || new_fd as usize >= MAX_FDS { return -1; }
    if old_fd == new_fd { return new_fd; }

    let entry = table.fds[old_fd as usize];
    // Close new_fd if it's open
    if table.fds[new_fd as usize].in_use {
        fd_free(table, new_fd);
    }
    table.fds[new_fd as usize] = FileDesc {
        inode_idx: entry.inode_idx, offset: entry.offset,
        flags: entry.flags, in_use: true,
    };
    if entry.inode_idx != NO_IDX && (entry.inode_idx as usize) < MAX_FILES {
        FS.lock().inodes[entry.inode_idx as usize].ref_count += 1;
    }
    new_fd
}

// ─── VFS I/O — accepts explicit FdTable (C used task_current() which is Phase 7) ─

/// Open a file.  Returns fd on success, -1 on error.
pub fn vfs_open(table: &mut FdTable, path: &[u8], flags: u32) -> i32 {
    let mut inode_idx = path_lookup(path);

    if inode_idx == NO_IDX {
        if flags & O_CREAT == 0 {
            // Try FAT32 proxy before giving up.
            inode_idx = try_fat32_open(path);
            if inode_idx == NO_IDX { return -1; }
        } else {
            // Try FAT32-backed creation first (path under a FAT32 mount).
            if let Some(fat_idx) = try_fat32_create(path) {
                // O_TRUNC on a freshly created inode is a no-op (size==0),
                // but mark it dirty so vfs_close flushes an explicit truncate.
                if flags & O_TRUNC != 0 {
                    FS.lock().inodes[fat_idx as usize].fat32_dirty = true;
                }
                return fd_alloc(table, fat_idx, flags);
            }

            // Fallback: create the file in ramfs.
            let (parent_idx, filename) = path_parent(path);
            if parent_idx == NO_IDX || filename.is_empty() { return -1; }

            inode_idx = inode_alloc(INODE_FILE, PERM_READ | PERM_WRITE);
            if inode_idx == NO_IDX { return -1; }

            if dir_add_entry(parent_idx, filename, inode_idx).is_err() {
                inode_free(inode_idx);
                return -1;
            }
        }
    }

    // Truncate on O_TRUNC.
    if flags & O_TRUNC != 0 {
        let itype = FS.lock().inodes[inode_idx as usize].itype;
        if itype == INODE_FILE {
            let _ = inode_resize(inode_idx, 0);
            // Mark dirty if this is an existing FAT32-backed inode.
            {
                let mut fs = FS.lock();
                if fs.inodes[inode_idx as usize].fat32_backed {
                    fs.inodes[inode_idx as usize].fat32_dirty = true;
                }
            }
        }
    }

    fd_alloc(table, inode_idx, flags)
}

/// Close a file descriptor.
///
/// If the underlying inode is FAT32-backed and dirty, the inode data is
/// flushed to disk before the file descriptor is released.
pub fn vfs_close(table: &mut FdTable, fd: i32) -> i32 {
    if fd < 0 || fd as usize >= MAX_FDS || !table.fds[fd as usize].in_use { return 0; }

    let inode_idx = table.fds[fd as usize].inode_idx;

    // Flush FAT32-backed dirty inodes.
    if inode_idx != NO_IDX && (inode_idx as usize) < MAX_FILES {
        // Snapshot fat32 state + data pointer under the lock, then release
        // before doing slow disk I/O.
        let (fat32_backed, fat32_dirty, fat32_name, data_ptr, size) = {
            let fs = FS.lock();
            let n  = &fs.inodes[inode_idx as usize];
            (n.fat32_backed, n.fat32_dirty, n.fat32_name, n.data, n.size)
        };

        if fat32_backed && fat32_dirty {
            // Safety: data_ptr is heap-allocated and the inode is kept alive
            // until fd_free() below.  fat32_write_file does not acquire FS lock.
            let slice: &[u8] = if size > 0 && !data_ptr.is_null() {
                unsafe { core::slice::from_raw_parts(data_ptr, size as usize) }
            } else {
                &[]
            };
            let _ = crate::fat32::fat32_write_file(&fat32_name, slice);
            FS.lock().inodes[inode_idx as usize].fat32_dirty = false;
        }
    }

    fd_free(table, fd);
    0
}

/// Read from a file descriptor.
/// Returns bytes read on success, -1 on error.
pub fn vfs_read(table: &mut FdTable, fd: i32, buf: *mut u8, count: usize) -> i32 {
    if fd < 0 || fd as usize >= MAX_FDS || !table.fds[fd as usize].in_use { return -1; }

    let fd_ent  = table.fds[fd as usize];
    let inode_idx = fd_ent.inode_idx;
    if inode_idx == NO_IDX { return -1; }

    // Extract inode type and device callback while lock is held.
    let (itype, dev_read_fn) = {
        let fs = FS.lock();
        let n  = &fs.inodes[inode_idx as usize];
        (n.itype, n.dev_read)
    };

    if itype == INODE_DEVICE {
        return if let Some(f) = dev_read_fn {
            unsafe { f(buf, count) }
        } else {
            -1
        };
    }
    if itype != INODE_FILE { return -1; }

    let (data_ptr, size) = {
        let fs = FS.lock();
        let n  = &fs.inodes[inode_idx as usize];
        (n.data, n.size)
    };

    let offset    = fd_ent.offset;
    let available = size.saturating_sub(offset) as usize;
    let to_read   = count.min(available);

    if to_read > 0 {
        unsafe { ptr::copy_nonoverlapping(data_ptr.add(offset as usize), buf, to_read); }
    }
    table.fds[fd as usize].offset += to_read as u32;
    to_read as i32
}

/// Write to a file descriptor.
/// Returns bytes written on success, -1 on error.
pub fn vfs_write(table: &mut FdTable, fd: i32, buf: *const u8, count: usize) -> i32 {
    if fd < 0 || fd as usize >= MAX_FDS || !table.fds[fd as usize].in_use { return -1; }

    let fd_ent    = table.fds[fd as usize];
    let inode_idx = fd_ent.inode_idx;
    if inode_idx == NO_IDX { return -1; }

    let (itype, dev_write_fn) = {
        let fs = FS.lock();
        let n  = &fs.inodes[inode_idx as usize];
        (n.itype, n.dev_write)
    };

    if itype == INODE_DEVICE {
        return if let Some(f) = dev_write_fn {
            unsafe { f(buf, count) }
        } else {
            -1
        };
    }
    if itype != INODE_FILE { return -1; }

    // Append mode: seek to end.
    if fd_ent.flags & O_APPEND != 0 {
        let size = FS.lock().inodes[inode_idx as usize].size;
        table.fds[fd as usize].offset = size;
    }

    let offset = table.fds[fd as usize].offset;

    // `count` is a usize but every size/offset in the inode table is a u32.
    // `count as u32` silently truncated: a 4 GiB + 1 write sized the buffer
    // for 1 byte and then `copy_nonoverlapping` below copied the full usize,
    // smashing the heap past the allocation. The `offset + count` addition was
    // also unchecked, so with `overflow-checks = true` a large offset aborted
    // the kernel instead. `sys_write` clamps to 4096 so ring 3 cannot reach
    // this, but in-kernel callers pass their own lengths.
    let count_u32 = match u32::try_from(count) {
        Ok(c)  => c,
        Err(_) => return -1,
    };
    let new_size = match offset.checked_add(count_u32) {
        Some(v) => v,
        None    => return -1,
    };

    // Grow data buffer if necessary.
    let cap = FS.lock().inodes[inode_idx as usize].capacity;
    if new_size > cap {
        if inode_resize(inode_idx, new_size).is_err() { return -1; }
    } else {
        // Just update the logical size if needed.
        let mut fs = FS.lock();
        if new_size > fs.inodes[inode_idx as usize].size {
            fs.inodes[inode_idx as usize].size = new_size;
        }
    }

    // Re-read the capacity after the (possible) resize and clamp the copy to
    // what the allocation actually holds, so a resize that returned Ok with a
    // smaller-than-requested buffer still cannot be overrun.
    let (data_ptr, cap_now) = {
        let fs = FS.lock();
        let n  = &fs.inodes[inode_idx as usize];
        (n.data, n.capacity)
    };
    let writable = cap_now.saturating_sub(offset) as usize;
    let to_write = count.min(writable);
    if to_write > 0 {
        unsafe { ptr::copy_nonoverlapping(buf, data_ptr.add(offset as usize), to_write); }
    }
    // `offset + to_write <= offset + count == new_size`, already checked above.
    table.fds[fd as usize].offset = offset + to_write as u32;

    // Mark FAT32-backed inodes dirty so vfs_close flushes them.
    {
        let mut fs = FS.lock();
        if fs.inodes[inode_idx as usize].fat32_backed {
            fs.inodes[inode_idx as usize].fat32_dirty = true;
        }
    }

    // Report what was actually copied, not what was asked for.
    to_write as i32
}

/// Seek within a file.  Returns the new offset, or -1 on error.
pub fn vfs_lseek(table: &mut FdTable, fd: i32, offset: i32, whence: i32) -> i32 {
    if fd < 0 || fd as usize >= MAX_FDS || !table.fds[fd as usize].in_use { return -1; }

    let inode_idx = table.fds[fd as usize].inode_idx;
    if inode_idx == NO_IDX { return -1; }

    let size = FS.lock().inodes[inode_idx as usize].size;
    let cur  = table.fds[fd as usize].offset;

    let new_offset = match whence {
        SEEK_SET => offset as u32,
        SEEK_CUR => cur.wrapping_add(offset as u32),
        SEEK_END => size.wrapping_add(offset as u32),
        _        => return -1,
    };
    if new_offset > size { return -1; }

    table.fds[fd as usize].offset = new_offset;
    new_offset as i32
}

// ─── Device callbacks ─────────────────────────────────────────────────────────

unsafe fn device_stdin_read(_buf: *mut u8, _count: usize) -> i32 {
    0   // No keyboard input in Phase 6
}

unsafe fn device_stdout_write(buf: *const u8, count: usize) -> i32 {
    for i in 0..count {
        robot_os_drivers::uart::putc(*buf.add(i));
    }
    count as i32
}

// ─── Filesystem initialisation — port of `fs_init()` in fs.c ─────────────────

/// Initialise the ramfs.  Creates root "/", "/dev", and the three std device
/// files.  Must be called after the heap is initialised.
pub fn init() {
    // Root inode "/"
    let root_idx = inode_alloc(INODE_DIR, PERM_READ | PERM_WRITE | PERM_EXEC);
    assert!(root_idx != NO_IDX, "[FS] inode pool exhausted for root");
    FS.lock().root_idx = root_idx;

    // /dev
    let dev_idx = inode_alloc(INODE_DIR, PERM_READ | PERM_EXEC);
    dir_add_entry(root_idx, b"dev", dev_idx)
        .expect("[FS] Failed to create /dev");

    // /dev/stdin
    let stdin_idx = inode_alloc(INODE_DEVICE, PERM_READ);
    FS.lock().inodes[stdin_idx as usize].dev_read = Some(device_stdin_read);
    dir_add_entry(dev_idx, b"stdin", stdin_idx)
        .expect("[FS] Failed to create /dev/stdin");

    // /dev/stdout
    let stdout_idx = inode_alloc(INODE_DEVICE, PERM_WRITE);
    FS.lock().inodes[stdout_idx as usize].dev_write = Some(device_stdout_write);
    dir_add_entry(dev_idx, b"stdout", stdout_idx)
        .expect("[FS] Failed to create /dev/stdout");

    // /dev/stderr (same write callback as stdout)
    let stderr_idx = inode_alloc(INODE_DEVICE, PERM_WRITE);
    FS.lock().inodes[stderr_idx as usize].dev_write = Some(device_stdout_write);
    dir_add_entry(dev_idx, b"stderr", stderr_idx)
        .expect("[FS] Failed to create /dev/stderr");
}
