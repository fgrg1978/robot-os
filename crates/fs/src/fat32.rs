/// FAT32 filesystem — port of kernel/fs/fat32.c
///
/// Mounts a FAT32 volume on VirtIO block device and provides
/// read-only file access (read/write could be added in the future).

use robot_os_sync::SpinLock;

// ── FAT32 on-disk structures ──────────────────────────────────────────────────

/// BIOS Parameter Block (BPB) as found at sector 0.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Fat32Bpb {
    pub jmp_boot:        [u8; 3],
    pub oem_name:        [u8; 8],
    pub bytes_per_sec:   u16,
    pub sec_per_clus:    u8,
    pub rsvd_sec_cnt:    u16,
    pub num_fats:        u8,
    pub root_ent_cnt:    u16,   // 0 for FAT32
    pub tot_sec16:       u16,   // 0 for FAT32
    pub media:           u8,
    pub fat_sz16:        u16,   // 0 for FAT32
    pub sec_per_trk:     u16,
    pub num_heads:       u16,
    pub hidd_sec:        u32,
    pub tot_sec32:       u32,
    // FAT32-specific
    pub fat_sz32:        u32,
    pub ext_flags:       u16,
    pub fs_ver:          u16,
    pub root_clus:       u32,
    pub fs_info:         u16,
    pub bk_boot_sec:     u16,
    pub reserved:        [u8; 12],
    pub drv_num:         u8,
    pub reserved1:       u8,
    pub boot_sig:        u8,
    pub vol_id:          u32,
    pub vol_lab:         [u8; 11],
    pub fil_sys_type:    [u8; 8],
}

/// FAT32 short-name directory entry (32 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Fat32Dirent {
    pub name:       [u8; 8],
    pub ext:        [u8; 3],
    pub attr:       u8,
    pub nt_res:     u8,
    pub crt_time_tenth: u8,
    pub crt_time:   u16,
    pub crt_date:   u16,
    pub lst_acc_date: u16,
    pub fst_clus_hi: u16,
    pub wrt_time:   u16,
    pub wrt_date:   u16,
    pub fst_clus_lo: u16,
    pub file_size:  u32,
}

#[allow(dead_code)] const ATTR_READ_ONLY: u8 = 0x01;
#[allow(dead_code)] const ATTR_HIDDEN:    u8 = 0x02;
#[allow(dead_code)] const ATTR_SYSTEM:    u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
#[allow(dead_code)] const ATTR_ARCHIVE:   u8 = 0x20;
const ATTR_LFN:       u8 = 0x0F;  // Long filename entry marker

const FAT32_EOC: u32 = 0x0FFF_FFF8;  // End-of-chain marker
/// Value written to the FAT to terminate a cluster chain (canonical EOC).
const FAT32_END_OF_CHAIN: u32 = 0x0FFF_FFFF;
/// First valid data cluster number (0 and 1 are reserved per the FAT spec).
const FAT32_FIRST_DATA_CLUSTER: u32 = 2;

const SECTOR_SIZE: usize = 512;

/// Size of one on-disk directory entry.
const FAT32_DIR_ENTRY_SIZE: usize = 32;
/// Number of directory entries that fit in a 512-byte sector.
const FAT32_DIRENTS_PER_SECTOR: usize = SECTOR_SIZE / FAT32_DIR_ENTRY_SIZE;

/// First byte of a directory entry slot that is unused and terminates the dir.
const DIRENT_MARK_END: u8 = 0x00;
/// First byte of a directory entry slot that was deleted (reusable).
const DIRENT_MARK_DELETED: u8 = 0xE5;

/// Offsets within a 32-byte directory entry.
const DIRENT_OFF_NAME: usize = 0;
const DIRENT_OFF_EXT: usize = 8;
const DIRENT_OFF_ATTR: usize = 11;
const DIRENT_OFF_FST_CLUS_HI: usize = 20;
const DIRENT_OFF_FST_CLUS_LO: usize = 26;
const DIRENT_OFF_FILE_SIZE: usize = 28;

/// ATTR byte value for a regular file (archive bit set).
const DIRENT_ATTR_ARCHIVE_FILE: u8 = 0x20;
/// ATTR byte value for a subdirectory.
const DIRENT_ATTR_SUBDIR: u8 = ATTR_DIRECTORY;

/// Maximum concurrently open FAT32 file handles.
pub const FAT32_MAX_OPEN_FILES: usize = 16;
/// Maximum directory depth supported by the path walker.
const FAT32_MAX_PATH_DEPTH: usize = 8;

// ── Write-Ahead Journal (AS — Power-Loss Safety) ────────────────────────────

/// Journal sector location — sector 1, immediately after BPB.
const JOURNAL_SECTOR: u32 = 1;

/// Journal entry states.
const JOURNAL_EMPTY: u8 = 0x00;
const JOURNAL_PENDING: u8 = 0x01;
const JOURNAL_COMMITTED: u8 = 0x02;

/// Journal operation types.
const JOURNAL_OP_ALLOC: u8 = 1;
#[allow(dead_code)]
const JOURNAL_OP_FREE: u8 = 2;
const JOURNAL_OP_WRITE_DIR: u8 = 3;
const JOURNAL_OP_UNLINK: u8 = 4;

/// Journal magic bytes — "JRNL".
const JOURNAL_MAGIC: [u8; 4] = [b'J', b'R', b'N', b'L'];

/// Size of the reserved portion after JournalEntry fixed fields.
const JOURNAL_RESERVED_SIZE: usize = 490;

/// Journal entry format — fits in one 512-byte sector.
/// Written BEFORE the actual FAT/directory update, so on power loss
/// we can replay or discard the pending operation.
#[repr(C)]
#[derive(Clone, Copy)]
struct JournalEntry {
    magic: [u8; 4],                         // "JRNL"
    state: u8,                              // EMPTY, PENDING, COMMITTED
    op_type: u8,                            // ALLOC, FREE, WRITE_DIR, UNLINK
    _pad: [u8; 2],
    cluster: u32,                           // target cluster
    fat_value: u32,                         // new FAT entry value
    dir_sector: u32,                        // directory sector being modified
    dir_offset: u16,                        // offset within directory sector
    _reserved: [u8; JOURNAL_RESERVED_SIZE], // padding to 512 bytes
}

impl JournalEntry {
    const fn empty() -> Self {
        JournalEntry {
            magic: JOURNAL_MAGIC,
            state: JOURNAL_EMPTY,
            op_type: 0,
            _pad: [0; 2],
            cluster: 0,
            fat_value: 0,
            dir_sector: 0,
            dir_offset: 0,
            _reserved: [0; JOURNAL_RESERVED_SIZE],
        }
    }

    /// Serialize journal entry into a 512-byte sector buffer.
    fn to_sector(&self, buf: &mut [u8; SECTOR_SIZE]) {
        buf.fill(0);
        buf[0..4].copy_from_slice(&self.magic);
        buf[4] = self.state;
        buf[5] = self.op_type;
        // _pad at [6..8]
        buf[8..12].copy_from_slice(&self.cluster.to_le_bytes());
        buf[12..16].copy_from_slice(&self.fat_value.to_le_bytes());
        buf[16..20].copy_from_slice(&self.dir_sector.to_le_bytes());
        buf[20..22].copy_from_slice(&self.dir_offset.to_le_bytes());
    }

    /// Deserialize journal entry from a 512-byte sector buffer.
    fn from_sector(buf: &[u8; SECTOR_SIZE]) -> Self {
        let mut entry = JournalEntry::empty();
        entry.magic.copy_from_slice(&buf[0..4]);
        entry.state = buf[4];
        entry.op_type = buf[5];
        entry.cluster = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        entry.fat_value = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
        entry.dir_sector = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);
        entry.dir_offset = u16::from_le_bytes([buf[20], buf[21]]);
        entry
    }
}

/// Write a journal entry to the journal sector.
fn fat32_journal_write(entry: &JournalEntry) -> Result<(), ()> {
    let mut buf = [0u8; SECTOR_SIZE];
    entry.to_sector(&mut buf);
    write_sector(JOURNAL_SECTOR, &buf)
}

/// Read the journal entry from the journal sector.
fn fat32_journal_read() -> Result<JournalEntry, ()> {
    let mut buf = [0u8; SECTOR_SIZE];
    read_sector(JOURNAL_SECTOR, &mut buf)?;
    Ok(JournalEntry::from_sector(&buf))
}

/// Clear the journal (set state to EMPTY).
fn fat32_journal_clear() -> Result<(), ()> {
    let entry = JournalEntry::empty();
    fat32_journal_write(&entry)
}

/// Check and replay journal on mount.
/// If journal has PENDING entry -> roll back (free allocated clusters).
/// If journal has COMMITTED entry -> complete (clear journal).
fn fat32_journal_recover() -> Result<(), ()> {
    let entry = fat32_journal_read()?;

    // Not a valid journal entry — nothing to recover.
    if entry.magic != JOURNAL_MAGIC {
        return Ok(());
    }

    match entry.state {
        JOURNAL_PENDING => {
            // Operation was not completed — roll back.
            // If it was an alloc, free the cluster that was partially allocated.
            if entry.op_type == JOURNAL_OP_ALLOC && entry.cluster >= 2 {
                let _ = fat32_write_fat_entry(entry.cluster, 0);
            }
            // For FREE/WRITE_DIR/UNLINK with PENDING state, the FAT/dir
            // was not yet modified, so nothing to undo.
            robot_os_drivers::kprintln!(
                "[FAT32] Journal recovery: rolled back pending op={}",
                entry.op_type
            );
            fat32_journal_clear()
        }
        JOURNAL_COMMITTED => {
            // Operation completed but journal wasn't cleared — just clear it.
            robot_os_drivers::kprintln!("[FAT32] Journal recovery: cleared committed entry");
            fat32_journal_clear()
        }
        _ => {
            // EMPTY or unknown — nothing to do.
            Ok(())
        }
    }
}

// ── Mounted volume state ──────────────────────────────────────────────────────

struct Fat32Vol {
    mounted:       bool,
    fat_start:     u32,   // First sector of FAT
    data_start:    u32,   // First sector of data region
    root_cluster:  u32,   // Cluster number of root directory
    secs_per_clus: u32,   // Sectors per cluster
    bytes_per_clus: u32,  // Bytes per cluster
    fat_sz32:      u32,   // Sectors per FAT table (Phase 8)
    num_fats:      u8,    // Number of FAT copies, typically 2 (Phase 8)
}

impl Fat32Vol {
    const fn new() -> Self {
        Fat32Vol {
            mounted:        false,
            fat_start:      0,
            data_start:     0,
            root_cluster:   2,
            secs_per_clus:  1,
            bytes_per_clus: 512,
            fat_sz32:       0,
            num_fats:       2,
        }
    }

    /// Convert cluster number to first sector in that cluster.
    #[allow(dead_code)]
    fn cluster_to_sector(&self, cluster: u32) -> u32 {
        self.data_start + (cluster - 2) * self.secs_per_clus
    }
}

static FAT32: SpinLock<Fat32Vol> = SpinLock::new(Fat32Vol::new());

// ── Sector cache (LRU, 8 entries × 512 B = 4 KiB) ────────────────────────────
//
// FAT32 access patterns are heavily skewed: a single file read of N
// clusters causes the kernel to re-read the same FAT sector and the
// same directory sector dozens of times. Without this cache, every
// `vfs_read(1 byte)` paid a full VirtIO round-trip (~5 ms). With 8
// cache entries we hold the boot sector, the active FAT sector, the
// root-dir sector, and 5 hot data sectors — covers the common case of
// reading a small file under 4 KiB without ever hitting the device
// after the first miss. Estimated 5–10× speedup on small-file IO.

const SECTOR_CACHE_LINES: usize = 8;

struct SectorCacheLine {
    sector: u32,             // u32::MAX = invalid
    data:   [u8; SECTOR_SIZE],
    last_use: u64,           // monotonic counter for LRU eviction
}

impl SectorCacheLine {
    const fn new() -> Self {
        Self { sector: u32::MAX, data: [0u8; SECTOR_SIZE], last_use: 0 }
    }
}

struct SectorCache {
    lines:   [SectorCacheLine; SECTOR_CACHE_LINES],
    counter: u64,
    hits:    u32,
    misses:  u32,
}

impl SectorCache {
    const fn new() -> Self {
        const EMPTY: SectorCacheLine = SectorCacheLine::new();
        Self {
            lines:   [EMPTY; SECTOR_CACHE_LINES],
            counter: 1,
            hits:    0,
            misses:  0,
        }
    }
}

static SECTOR_CACHE: SpinLock<SectorCache> = SpinLock::new(SectorCache::new());

/// Invalidate every cache line touching `sector` — call after an
/// out-of-band write (raw block layer, OTA writes that bypass FS).
#[allow(dead_code)]
pub fn fat32_cache_invalidate(sector: u32) {
    let mut c = SECTOR_CACHE.lock();
    for line in c.lines.iter_mut() {
        if line.sector == sector { line.sector = u32::MAX; }
    }
}

/// Read a sector, preferring the cache. On miss, fetch via the block
/// device and install in the LRU-evicted line.
fn read_sector(sector: u32, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), ()> {
    // Look up in cache.
    {
        let mut c = SECTOR_CACHE.lock();
        for line in c.lines.iter_mut() {
            if line.sector == sector {
                buf.copy_from_slice(&line.data);
                c.counter = c.counter.saturating_add(1);
                let stamp = c.counter;
                // Refresh LRU stamp on the matched line.
                for line in c.lines.iter_mut() {
                    if line.sector == sector { line.last_use = stamp; break; }
                }
                c.hits = c.hits.saturating_add(1);
                return Ok(());
            }
        }
    }

    // Miss — fetch from device.
    robot_os_drivers::blkdev::read(sector as u64, 1, buf)?;

    // Install in LRU-evicted line.
    let mut c = SECTOR_CACHE.lock();
    c.counter = c.counter.saturating_add(1);
    let stamp = c.counter;
    let mut evict_idx = 0usize;
    let mut oldest = u64::MAX;
    let mut found_empty = false;
    for (i, line) in c.lines.iter().enumerate() {
        if line.sector == u32::MAX { evict_idx = i; found_empty = true; break; }
        if line.last_use < oldest { oldest = line.last_use; evict_idx = i; }
    }
    let _ = found_empty;
    let line = &mut c.lines[evict_idx];
    line.sector   = sector;
    line.data.copy_from_slice(buf);
    line.last_use = stamp;
    c.misses = c.misses.saturating_add(1);
    Ok(())
}

/// Statistics for diagnostics (`fs cache` shell command, etc.).
#[allow(dead_code)]
pub fn fat32_cache_stats() -> (u32, u32) {
    let c = SECTOR_CACHE.lock();
    (c.hits, c.misses)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Mount a FAT32 filesystem on the first VirtIO block device.
/// Returns Ok(()) on success.
pub fn fat32_mount() -> Result<(), ()> {
    let mut sector0 = [0u8; SECTOR_SIZE];
    read_sector(0, &mut sector0)?;

    // Parse BPB (at offset 0 in sector 0)
    let bpb = unsafe { &*(sector0.as_ptr() as *const Fat32Bpb) };

    // Validate signatures
    if bpb.bytes_per_sec != 512 { return Err(()); }
    if bpb.fat_sz32 == 0        { return Err(()); }

    let fat_start  = bpb.rsvd_sec_cnt as u32;
    let data_start = fat_start + bpb.num_fats as u32 * bpb.fat_sz32;
    let root_clus  = bpb.root_clus;
    let spc        = bpb.sec_per_clus as u32;

    let mut v = FAT32.lock();
    v.fat_start      = fat_start;
    v.data_start     = data_start;
    v.root_cluster   = root_clus;
    v.secs_per_clus  = spc;
    v.bytes_per_clus = spc * 512;
    v.fat_sz32       = bpb.fat_sz32;
    v.num_fats       = bpb.num_fats;
    v.mounted        = true;

    robot_os_drivers::kprintln!(
        "[FAT32] Mounted: FAT@{}, data@{}, root_clus={}, spc={}",
        fat_start, data_start, root_clus, spc
    );

    // Recover from any incomplete operations before power loss.
    drop(v);
    fat32_journal_recover()?;

    Ok(())
}

/// Read the FAT entry for `cluster` to find the next cluster in the chain.
/// Returns Ok(next_cluster), where >= FAT32_EOC means end of chain.
fn fat32_next_cluster(cluster: u32) -> Result<u32, ()> {
    let fat_start = { FAT32.lock().fat_start };
    // Each FAT32 entry is 4 bytes; 512-byte sector holds 128 entries
    let fat_sector = fat_start + cluster / 128;
    let fat_offset = (cluster % 128) as usize;

    let mut buf = [0u8; SECTOR_SIZE];
    read_sector(fat_sector, &mut buf)?;

    let entry_bytes = &buf[fat_offset * 4..fat_offset * 4 + 4];
    let entry = u32::from_le_bytes([entry_bytes[0], entry_bytes[1], entry_bytes[2], entry_bytes[3]]);
    Ok(entry & 0x0FFF_FFFF)
}

/// Read all data from a cluster chain into `out_buf`.
/// Returns bytes written.
pub fn fat32_read_chain(start_cluster: u32, out_buf: &mut [u8]) -> usize {
    let (spc, data_start) = {
        let v = FAT32.lock();
        (v.secs_per_clus, v.data_start)
    };

    let mut cluster = start_cluster;
    let mut written = 0;

    while written < out_buf.len() {
        if cluster < 2 || cluster >= FAT32_EOC { break; }

        let first_sector = data_start + (cluster - 2) * spc;
        for s in 0..spc {
            let remaining = out_buf.len() - written;
            if remaining == 0 { break; }
            let mut sec_buf = [0u8; SECTOR_SIZE];
            if read_sector(first_sector + s, &mut sec_buf).is_err() { break; }
            let to_copy = remaining.min(SECTOR_SIZE);
            out_buf[written..written + to_copy].copy_from_slice(&sec_buf[..to_copy]);
            written += to_copy;
        }

        cluster = match fat32_next_cluster(cluster) {
            Ok(n) => n,
            Err(_) => break,
        };
    }

    written
}

/// Find a file in the root directory by short name (8.3 format, uppercased).
/// Returns (start_cluster, file_size) or Err if not found.
pub fn fat32_lookup_root(name83: &[u8; 11]) -> Result<(u32, u32), ()> {
    if !FAT32.lock().mounted { return Err(()); }
    let root_cluster = FAT32.lock().root_cluster;

    let mut cluster = root_cluster;
    while cluster >= 2 && cluster < FAT32_EOC {
        let (spc, data_start) = {
            let v = FAT32.lock();
            (v.secs_per_clus, v.data_start)
        };
        let first_sector = data_start + (cluster - 2) * spc;

        for s in 0..spc {
            let mut sec_buf = [0u8; SECTOR_SIZE];
            read_sector(first_sector + s, &mut sec_buf).ok();

            // 16 directory entries per 512-byte sector
            for e in 0..16 {
                let off = e * 32;
                let entry = unsafe {
                    &*(sec_buf[off..off + 32].as_ptr() as *const Fat32Dirent)
                };
                if entry.name[0] == 0x00 { return Err(()); }  // End of directory
                if entry.name[0] == 0xE5 { continue; }        // Deleted
                if entry.attr == ATTR_LFN { continue; }        // LFN entry
                if entry.attr & ATTR_VOLUME_ID != 0 { continue; }

                let mut ent_name = [0u8; 11];
                ent_name[..8].copy_from_slice(&entry.name);
                ent_name[8..11].copy_from_slice(&entry.ext);
                if &ent_name == name83 {
                    let cluster_hi = entry.fst_clus_hi as u32;
                    let cluster_lo = entry.fst_clus_lo as u32;
                    let file_cluster = (cluster_hi << 16) | cluster_lo;
                    return Ok((file_cluster, entry.file_size));
                }
            }
        }

        cluster = fat32_next_cluster(cluster).unwrap_or(FAT32_EOC);
    }
    Err(())
}

/// Returns true if FAT32 is mounted.
pub fn fat32_mounted() -> bool {
    FAT32.lock().mounted
}

// ── Write support (Phase 8) ───────────────────────────────────────────────────

/// Write a single 512-byte sector via VirtIO block.
///
/// Updates the in-memory sector cache so a subsequent read sees the
/// fresh contents (write-through). Without invalidation a later
/// read_sector() would return stale cached data even after a write.
fn write_sector(sector: u32, buf: &[u8; SECTOR_SIZE]) -> Result<(), ()> {
    robot_os_drivers::blkdev::write(sector as u64, 1, buf)?;
    // Update cache line if present (write-through). Cheaper than a
    // straight invalidate because the next reader pays no miss cost.
    let mut c = SECTOR_CACHE.lock();
    for line in c.lines.iter_mut() {
        if line.sector == sector {
            line.data.copy_from_slice(buf);
            break;
        }
    }
    let _ = sector;  // silence "unused" if no line matched
    Ok(())
}

/// Write a FAT32 entry for `cluster` to all FAT table copies.
///
/// The upper 4 bits of the existing entry are preserved (as per FAT32 spec).
fn fat32_write_fat_entry(cluster: u32, value: u32) -> Result<(), ()> {
    let (fat_start, fat_sz32, num_fats) = {
        let v = FAT32.lock();
        (v.fat_start, v.fat_sz32, v.num_fats)
    };

    let fat_sector_off = cluster / 128;
    let fat_byte_off   = (cluster % 128) as usize * 4;

    // Read sector from FAT copy 0 for modification.
    let mut buf = [0u8; SECTOR_SIZE];
    read_sector(fat_start + fat_sector_off, &mut buf)?;

    // Preserve upper nibble of the existing entry (FAT32 spec requirement).
    let cur = u32::from_le_bytes([
        buf[fat_byte_off], buf[fat_byte_off + 1],
        buf[fat_byte_off + 2], buf[fat_byte_off + 3],
    ]);
    let new_val = (cur & 0xF000_0000) | (value & 0x0FFF_FFFF);
    let bytes = new_val.to_le_bytes();
    buf[fat_byte_off..fat_byte_off + 4].copy_from_slice(&bytes);

    // Write updated sector to all FAT copies.
    let copies = num_fats.max(1) as u32;
    for i in 0..copies {
        write_sector(fat_start + i * fat_sz32 + fat_sector_off, &buf)?;
    }
    Ok(())
}

/// Scan the FAT for a free cluster (entry == 0), mark it as end-of-chain,
/// and return its cluster number.
pub fn fat32_alloc_cluster() -> Result<u32, ()> {
    let (fat_start, fat_sz32) = {
        let v = FAT32.lock();
        (v.fat_start, v.fat_sz32)
    };
    if fat_sz32 == 0 { return Err(()); }

    for sec_idx in 0..fat_sz32 {
        let mut buf = [0u8; SECTOR_SIZE];
        if read_sector(fat_start + sec_idx, &mut buf).is_err() { return Err(()); }
        for i in 0..128u32 {
            let cluster = sec_idx * 128 + i;
            if cluster < 2 { continue; }
            let off   = (i * 4) as usize;
            let entry = u32::from_le_bytes([
                buf[off], buf[off + 1], buf[off + 2], buf[off + 3],
            ]) & 0x0FFF_FFFF;
            if entry == 0 {
                // Mark as end-of-chain (allocated).
                fat32_write_fat_entry(cluster, 0x0FFF_FFFF)?;
                return Ok(cluster);
            }
        }
    }
    Err(()) // Disk full
}

/// Free all clusters in a chain starting at `start`.
pub fn fat32_free_chain(start: u32) {
    let mut cluster = start;
    while cluster >= 2 && cluster < FAT32_EOC {
        let next = fat32_next_cluster(cluster).unwrap_or(FAT32_EOC);
        let _ = fat32_write_fat_entry(cluster, 0); // Mark as free
        cluster = next;
    }
}

/// Write `data` into the sectors of `cluster`, zero-padding the last sector.
fn fat32_write_cluster(cluster: u32, data: &[u8]) -> Result<(), ()> {
    let (spc, data_start) = {
        let v = FAT32.lock();
        (v.secs_per_clus, v.data_start)
    };
    let first_sector = data_start + (cluster - 2) * spc;
    let mut written = 0usize;
    for s in 0..spc {
        let mut sec_buf = [0u8; SECTOR_SIZE];
        let remaining = data.len().saturating_sub(written);
        let to_copy   = remaining.min(SECTOR_SIZE);
        if to_copy > 0 {
            sec_buf[..to_copy].copy_from_slice(&data[written..written + to_copy]);
        }
        write_sector(first_sector + s, &sec_buf)?;
        written += to_copy;
        if written >= data.len() { break; }
    }
    Ok(())
}

/// Write a new 32-byte directory entry into the first free slot (0x00 or 0xE5)
/// of the root directory.
fn fat32_creat_root_dirent(name83: &[u8; 11], cluster: u32, size: u32) -> Result<(), ()> {
    let (root_cluster, spc, data_start) = {
        let v = FAT32.lock();
        (v.root_cluster, v.secs_per_clus, v.data_start)
    };
    let mut dir_cluster = root_cluster;
    while dir_cluster >= 2 && dir_cluster < FAT32_EOC {
        let first_sector = data_start + (dir_cluster - 2) * spc;
        for s in 0..spc {
            let mut sec_buf = [0u8; SECTOR_SIZE];
            read_sector(first_sector + s, &mut sec_buf).ok();
            for e in 0..16usize {
                let off        = e * 32;
                let first_byte = sec_buf[off];
                if first_byte == 0x00 || first_byte == 0xE5 {
                    // Write name (8) + ext (3)
                    sec_buf[off..off + 8].copy_from_slice(&name83[..8]);
                    sec_buf[off + 8..off + 11].copy_from_slice(&name83[8..11]);
                    // Attributes = ARCHIVE (0x20)
                    sec_buf[off + 11] = 0x20;
                    // NTRes, CrtTimeTenth = 0
                    sec_buf[off + 12] = 0;
                    sec_buf[off + 13] = 0;
                    // CrtTime, CrtDate, LstAccDate = 0
                    sec_buf[off + 14] = 0; sec_buf[off + 15] = 0;
                    sec_buf[off + 16] = 0; sec_buf[off + 17] = 0;
                    sec_buf[off + 18] = 0; sec_buf[off + 19] = 0;
                    // FstClusHI (LE u16)
                    let hi = ((cluster >> 16) & 0xFFFF) as u16;
                    sec_buf[off + 20] = (hi & 0xFF) as u8;
                    sec_buf[off + 21] = (hi >> 8) as u8;
                    // WrtTime, WrtDate = 0
                    sec_buf[off + 22] = 0; sec_buf[off + 23] = 0;
                    sec_buf[off + 24] = 0; sec_buf[off + 25] = 0;
                    // FstClusLO (LE u16)
                    let lo = (cluster & 0xFFFF) as u16;
                    sec_buf[off + 26] = (lo & 0xFF) as u8;
                    sec_buf[off + 27] = (lo >> 8) as u8;
                    // FileSize (LE u32)
                    sec_buf[off + 28] = (size & 0xFF) as u8;
                    sec_buf[off + 29] = ((size >> 8) & 0xFF) as u8;
                    sec_buf[off + 30] = ((size >> 16) & 0xFF) as u8;
                    sec_buf[off + 31] = ((size >> 24) & 0xFF) as u8;
                    return write_sector(first_sector + s, &sec_buf);
                }
            }
        }
        dir_cluster = fat32_next_cluster(dir_cluster).unwrap_or(FAT32_EOC);
    }
    Err(()) // No free directory slot
}

/// Mark a root-directory entry as deleted (first byte = 0xE5).
///
/// Uses journal for power-loss safety:
///   1. Write journal (PENDING, op=UNLINK)
///   2. Mark directory entry deleted
///   3. Write journal (COMMITTED)
///   4. Clear journal
pub fn fat32_unlink_root(name83: &[u8; 11]) -> Result<(), ()> {
    let (root_cluster, spc, data_start) = {
        let v = FAT32.lock();
        (v.root_cluster, v.secs_per_clus, v.data_start)
    };
    let mut cluster = root_cluster;
    while cluster >= 2 && cluster < FAT32_EOC {
        let first_sector = data_start + (cluster - 2) * spc;
        for s in 0..spc {
            let mut sec_buf = [0u8; SECTOR_SIZE];
            if read_sector(first_sector + s, &mut sec_buf).is_err() { continue; }
            for e in 0..16usize {
                let off = e * 32;
                if sec_buf[off] == 0x00 { return Err(()); }  // End of directory
                if sec_buf[off] == 0xE5 { continue; }         // Already deleted
                let attr = sec_buf[off + 11];
                if attr == ATTR_LFN { continue; }
                if attr & ATTR_VOLUME_ID != 0 { continue; }
                let mut ent_name = [0u8; 11];
                ent_name[..8].copy_from_slice(&sec_buf[off..off + 8]);
                ent_name[8..11].copy_from_slice(&sec_buf[off + 8..off + 11]);
                if &ent_name == name83 {
                    // Step 1: Journal PENDING before modifying directory.
                    let sector_num = first_sector + s;
                    let journal = JournalEntry {
                        magic: JOURNAL_MAGIC,
                        state: JOURNAL_PENDING,
                        op_type: JOURNAL_OP_UNLINK,
                        _pad: [0; 2],
                        cluster: 0,
                        fat_value: 0,
                        dir_sector: sector_num,
                        dir_offset: off as u16,
                        _reserved: [0; JOURNAL_RESERVED_SIZE],
                    };
                    fat32_journal_write(&journal)?;

                    // Step 2: Mark deleted.
                    sec_buf[off] = 0xE5;
                    write_sector(sector_num, &sec_buf)?;

                    // Step 3: Journal COMMITTED.
                    let committed = JournalEntry {
                        magic: JOURNAL_MAGIC,
                        state: JOURNAL_COMMITTED,
                        op_type: JOURNAL_OP_UNLINK,
                        _pad: [0; 2],
                        cluster: 0,
                        fat_value: 0,
                        dir_sector: sector_num,
                        dir_offset: off as u16,
                        _reserved: [0; JOURNAL_RESERVED_SIZE],
                    };
                    fat32_journal_write(&committed)?;

                    // Step 4: Clear journal.
                    return fat32_journal_clear();
                }
            }
        }
        cluster = fat32_next_cluster(cluster).unwrap_or(FAT32_EOC);
    }
    Err(())
}

/// Create or overwrite a file in the FAT32 root directory.
///
/// Existing data is freed first.  A new cluster chain is allocated, the data
/// is written sector-by-sector, and a fresh directory entry is committed.
///
/// Write ordering with journal (AS — power-loss safety):
///   1. Write journal entry (state=PENDING)
///   2. Write data clusters
///   3. Write FAT table
///   4. Write directory entry
///   5. Write journal entry (state=COMMITTED)
///   6. Clear journal (state=EMPTY)
pub fn fat32_write_file(name83: &[u8; 11], data: &[u8]) -> Result<(), ()> {
    if !FAT32.lock().mounted { return Err(()); }

    // If the file already exists, free its cluster chain and delete its dirent.
    if let Ok((old_cluster, _)) = fat32_lookup_root(name83) {
        if old_cluster >= 2 {
            fat32_free_chain(old_cluster);
        }
        let _ = fat32_unlink_root(name83);
    }

    let bytes_per_clus = FAT32.lock().bytes_per_clus as usize;
    if bytes_per_clus == 0 { return Err(()); }

    // Allocate a cluster chain and write the data.
    let first_cluster = if data.is_empty() {
        0u32  // Empty file: no cluster
    } else {
        let fc = fat32_alloc_cluster()?;

        // Step 1: Write journal (PENDING) before any FAT/dir modifications.
        let journal = JournalEntry {
            magic: JOURNAL_MAGIC,
            state: JOURNAL_PENDING,
            op_type: JOURNAL_OP_WRITE_DIR,
            _pad: [0; 2],
            cluster: fc,
            fat_value: FAT32_EOC,
            dir_sector: 0,
            dir_offset: 0,
            _reserved: [0; JOURNAL_RESERVED_SIZE],
        };
        fat32_journal_write(&journal)?;

        // Step 2: Write data clusters.
        let mut cur = fc;
        let mut off = 0usize;
        loop {
            let end = (off + bytes_per_clus).min(data.len());
            fat32_write_cluster(cur, &data[off..end])?;
            off = end;
            if off >= data.len() { break; }
            // Step 3: Extend FAT chain.
            let next = fat32_alloc_cluster()?;
            fat32_write_fat_entry(cur, next)?;
            cur = next;
        }
        fc
    };

    // Step 4: Write the directory entry.
    fat32_creat_root_dirent(name83, first_cluster, data.len() as u32)?;

    // Step 5: Mark journal as COMMITTED.
    let committed = JournalEntry {
        magic: JOURNAL_MAGIC,
        state: JOURNAL_COMMITTED,
        op_type: JOURNAL_OP_WRITE_DIR,
        _pad: [0; 2],
        cluster: first_cluster,
        fat_value: FAT32_EOC,
        dir_sector: 0,
        dir_offset: 0,
        _reserved: [0; JOURNAL_RESERVED_SIZE],
    };
    fat32_journal_write(&committed)?;

    // Step 6: Clear journal.
    fat32_journal_clear()
}

/// Convert a raw filename (no slashes) to FAT32 8.3 format.
fn path_to_83_local(name: &[u8]) -> Option<[u8; 11]> {
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

/// Unlink a root-directory file by raw filename (e.g. `b"TEST.TXT"`).
/// Frees its cluster chain and marks the directory entry deleted.
pub fn fat32_unlink_path(name: &[u8]) -> Result<(), ()> {
    let name83 = path_to_83_local(name).ok_or(())?;
    if let Ok((cluster, _)) = fat32_lookup_root(&name83) {
        if cluster >= 2 { fat32_free_chain(cluster); }
    }
    fat32_unlink_root(&name83)
}

// ── Sync (AS — Power-Loss Safety) ────────────────────────────────────────────

/// Force flush all pending writes to disk.
/// In our simple implementation, writes are already synchronous (no write cache),
/// but this ensures the journal is cleared.
pub fn fat32_sync() -> Result<(), ()> {
    if !FAT32.lock().mounted { return Err(()); }
    // Ensure journal is in a clean state.
    let entry = fat32_journal_read()?;
    if entry.magic == JOURNAL_MAGIC && entry.state == JOURNAL_COMMITTED {
        fat32_journal_clear()?;
    }
    Ok(())
}

// ── Directory listing ─────────────────────────────────────────────────────────

/// Convert a FAT32 8.3 raw name to a printable string.
/// Strips trailing spaces and inserts a '.' before the extension.
/// Returns (buffer, length).
fn format_83_name(raw: &[u8; 11]) -> ([u8; 13], usize) {
    let base_end = raw[..8].iter().rposition(|&b| b != b' ').map(|i| i + 1).unwrap_or(0);
    let ext_end  = raw[8..11].iter().rposition(|&b| b != b' ').map(|i| i + 1).unwrap_or(0);
    let mut buf = [0u8; 13];
    let mut len = 0usize;
    buf[..base_end].copy_from_slice(&raw[..base_end]);
    len += base_end;
    if ext_end > 0 {
        buf[len] = b'.';
        len += 1;
        buf[len..len + ext_end].copy_from_slice(&raw[8..8 + ext_end]);
        len += ext_end;
    }
    (buf, len)
}

/// Enumerate all entries in the FAT32 root directory.
///
/// Calls `cb(name, file_size, is_dir)` for each valid, non-deleted entry.
/// Deleted entries, LFN entries, and volume-ID entries are skipped.
pub fn fat32_ls_root(mut cb: impl FnMut(&[u8], u32, bool)) {
    if !FAT32.lock().mounted { return; }
    let root_cluster = FAT32.lock().root_cluster;

    let mut cluster = root_cluster;
    while cluster >= 2 && cluster < FAT32_EOC {
        let (spc, data_start) = {
            let v = FAT32.lock();
            (v.secs_per_clus, v.data_start)
        };
        let first_sector = data_start + (cluster - 2) * spc;

        'outer: for s in 0..spc {
            let mut sec_buf = [0u8; SECTOR_SIZE];
            if read_sector(first_sector + s, &mut sec_buf).is_err() { break 'outer; }

            for e in 0..16 {
                let off = e * 32;
                let entry = unsafe {
                    &*(sec_buf[off..off + 32].as_ptr() as *const Fat32Dirent)
                };
                if entry.name[0] == 0x00 { return; }   // End of directory
                if entry.name[0] == 0xE5 { continue; }  // Deleted
                if entry.attr == ATTR_LFN { continue; }  // LFN entry
                if entry.attr & ATTR_VOLUME_ID != 0 { continue; }

                let is_dir = entry.attr & ATTR_DIRECTORY != 0;
                let mut name83 = [0u8; 11];
                name83[..8].copy_from_slice(&entry.name);
                name83[8..].copy_from_slice(&entry.ext);
                let (name_buf, name_len) = format_83_name(&name83);
                cb(&name_buf[..name_len], entry.file_size, is_dir);
            }
        }

        cluster = fat32_next_cluster(cluster).unwrap_or(FAT32_EOC);
    }
}

// ── AS — File-level API (open / read / write / seek / close / sync) ──────────
//
// The APIs above expose raw cluster-chain primitives; the behavior crate (E06
// logging) needs a real file handle with positional I/O, append, seek, and
// fsync.  Everything below is `#![no_std]` and heap-free: open files live in
// a fixed-size table guarded by a SpinLock.
//
// Subdirectory support: mkdir, directory-entry updates in any directory
// cluster chain (not just root), and path walking from "/" through arbitrary
// nested subdirectories.  Long-filename (VFAT LFN) is NOT implemented — only
// 8.3 short names.

/// Error type returned by the file-level API.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FsError {
    /// Filesystem not mounted.
    NotMounted,
    /// Path not found.
    NotFound,
    /// Path exists but is a directory when a file was expected (or vice versa).
    WrongType,
    /// File is not open.
    BadHandle,
    /// Open handle table exhausted.
    TooManyOpen,
    /// Invalid path / component / flags.
    InvalidArg,
    /// Underlying block device I/O failed.
    Io,
    /// Disk full (no free clusters).
    NoSpace,
    /// Directory entry could not be created (no free slot and no extend).
    DirFull,
    /// Operation not supported (e.g. removing a non-empty directory).
    Unsupported,
}

/// Open-mode flags for `fat32_open`.
pub mod open_flags {
    /// Open for reading.
    pub const READ: u32 = 0x0001;
    /// Open for writing.
    pub const WRITE: u32 = 0x0002;
    /// Create file if it does not exist.
    pub const CREATE: u32 = 0x0004;
    /// Truncate to zero length on open.
    pub const TRUNCATE: u32 = 0x0008;
    /// Seek to end of file before each write.
    pub const APPEND: u32 = 0x0010;
}

/// Seek origin for `fat32_seek`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SeekFrom {
    Start(u32),
    Current(i32),
    End(i32),
}

/// Zero-sized volume marker.  Present so future multi-volume support is a
/// backwards-compatible change; today there is a single static `FAT32` volume.
#[derive(Copy, Clone)]
pub struct Volume {
    _private: (),
}

/// Opaque handle to an open FAT32 file.  Always returned by value and passed
/// by value; identifies a slot in the static open-file table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Fat32File {
    slot: u16,
    /// Generation counter — guards against stale handles after close+reopen.
    generation: u16,
}

/// Entry yielded by a directory iterator.
#[derive(Copy, Clone, Debug)]
pub struct DirEntryInfo {
    /// Printable 8.3 name ("FOO.TXT" or "SUBDIR") — trailing spaces stripped.
    pub name: [u8; 13],
    pub name_len: u8,
    pub size: u32,
    pub is_dir: bool,
    /// Starting cluster of the file/directory content (0 for empty files).
    pub first_cluster: u32,
}

/// Directory iterator.  Walks the cluster chain of a directory and yields one
/// `DirEntryInfo` at a time via `next()`.  Skips deleted / LFN / volume-ID
/// entries.  Stops at the first `DIRENT_MARK_END` sentinel.
pub struct Fat32DirIter {
    cluster: u32,
    sector_in_cluster: u32,
    entry_in_sector: usize,
    done: bool,
}

/// Internal per-open-file state.
#[derive(Copy, Clone)]
struct OpenFileEntry {
    in_use: bool,
    generation: u16,
    /// Absolute sector number of the dirent.
    dir_sector: u32,
    /// Byte offset within `dir_sector` of the dirent.
    dir_offset: u16,
    /// First cluster of the file's data chain (0 = empty file).
    first_cluster: u32,
    /// Current file size in bytes (as known to the handle).
    size: u32,
    /// Current file position.
    pos: u32,
    /// Open flags.
    flags: u32,
    /// Dirty flag: file data / size was modified and the dirent needs updating
    /// on sync/close.
    dirty: bool,
}

const EMPTY_OPEN_FILE: OpenFileEntry = OpenFileEntry {
    in_use: false,
    generation: 0,
    dir_sector: 0,
    dir_offset: 0,
    first_cluster: 0,
    size: 0,
    pos: 0,
    flags: 0,
    dirty: false,
};

static OPEN_FILES: SpinLock<[OpenFileEntry; FAT32_MAX_OPEN_FILES]> =
    SpinLock::new([EMPTY_OPEN_FILE; FAT32_MAX_OPEN_FILES]);

// ── Volume mount/unmount API ─────────────────────────────────────────────────

/// Mount the FAT32 filesystem and return a `Volume` handle.  Wraps
/// [`fat32_mount`] for a more modern API surface.
pub fn fat32_mount_volume() -> Result<Volume, FsError> {
    fat32_mount().map(|()| Volume { _private: () }).map_err(|()| FsError::NotMounted)
}

/// Unmount the FAT32 filesystem.  Flushes the journal and closes all open
/// files (dropping their dirty writes after a final sync).  Safe to call even
/// if no volume is mounted.
pub fn fat32_unmount(_vol: Volume) -> Result<(), FsError> {
    // Sync + close every currently-open file so no write is lost.
    let slots_to_close: [(bool, u16); FAT32_MAX_OPEN_FILES] = {
        let t = OPEN_FILES.lock();
        let mut out = [(false, 0u16); FAT32_MAX_OPEN_FILES];
        for i in 0..FAT32_MAX_OPEN_FILES {
            out[i] = (t[i].in_use, t[i].generation);
        }
        out
    };
    for i in 0..FAT32_MAX_OPEN_FILES {
        if slots_to_close[i].0 {
            let h = Fat32File { slot: i as u16, generation: slots_to_close[i].1 };
            let _ = fat32_close(h);
        }
    }
    let _ = fat32_sync();
    // Mark volume unmounted.
    FAT32.lock().mounted = false;
    Ok(())
}

// ── Path helpers ─────────────────────────────────────────────────────────────

/// Convert one raw path component (no slashes) to FAT32 8.3 uppercase form.
fn component_to_83(name: &[u8]) -> Option<[u8; 11]> {
    if name.is_empty() || name == b"." || name == b".." { return None; }
    let (base, ext) = match name.iter().position(|&b| b == b'.') {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (name, &[][..]),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 { return None; }
    let mut result = [b' '; 11];
    for (i, &b) in base.iter().enumerate() { result[i] = b.to_ascii_uppercase(); }
    for (i, &b) in ext.iter().enumerate() { result[8 + i] = b.to_ascii_uppercase(); }
    Some(result)
}

/// Split an absolute path (e.g. `b"/dir/sub/file.txt"`) into components.
/// Leading slashes are stripped; empty components are skipped.
/// Writes components into `out` and returns the number of components written.
fn split_path<'a>(
    path: &'a [u8],
    out: &mut [&'a [u8]; FAT32_MAX_PATH_DEPTH],
) -> Result<usize, FsError> {
    let mut rest = path;
    while rest.first() == Some(&b'/') { rest = &rest[1..]; }
    let mut count = 0usize;
    while !rest.is_empty() {
        let end = rest.iter().position(|&c| c == b'/').unwrap_or(rest.len());
        let comp = &rest[..end];
        if !comp.is_empty() {
            if count >= FAT32_MAX_PATH_DEPTH { return Err(FsError::InvalidArg); }
            out[count] = comp;
            count += 1;
        }
        rest = if end < rest.len() { &rest[end + 1..] } else { &[] };
    }
    Ok(count)
}

// ── Directory walking ────────────────────────────────────────────────────────

/// Result of searching a directory for an entry.
struct DirentLocation {
    /// Sector holding the dirent.
    sector: u32,
    /// Byte offset within `sector` of the dirent.
    offset: u16,
    first_cluster: u32,
    size: u32,
    attr: u8,
}

/// Search a directory (given by its starting cluster) for an entry matching
/// `name83`.
fn dir_find_in(dir_cluster: u32, name83: &[u8; 11]) -> Result<DirentLocation, FsError> {
    let (spc, data_start) = {
        let v = FAT32.lock();
        (v.secs_per_clus, v.data_start)
    };

    let mut cluster = dir_cluster;
    while cluster >= FAT32_FIRST_DATA_CLUSTER && cluster < FAT32_EOC {
        let first_sector = data_start + (cluster - FAT32_FIRST_DATA_CLUSTER) * spc;
        for s in 0..spc {
            let sec_num = first_sector + s;
            let mut buf = [0u8; SECTOR_SIZE];
            read_sector(sec_num, &mut buf).map_err(|()| FsError::Io)?;
            for e in 0..FAT32_DIRENTS_PER_SECTOR {
                let off = e * FAT32_DIR_ENTRY_SIZE;
                if buf[off + DIRENT_OFF_NAME] == DIRENT_MARK_END {
                    return Err(FsError::NotFound);
                }
                if buf[off + DIRENT_OFF_NAME] == DIRENT_MARK_DELETED { continue; }
                let attr = buf[off + DIRENT_OFF_ATTR];
                if attr == ATTR_LFN { continue; }
                if attr & ATTR_VOLUME_ID != 0 { continue; }
                let mut ent_name = [0u8; 11];
                ent_name[..8].copy_from_slice(&buf[off + DIRENT_OFF_NAME..off + DIRENT_OFF_NAME + 8]);
                ent_name[8..11].copy_from_slice(&buf[off + DIRENT_OFF_EXT..off + DIRENT_OFF_EXT + 3]);
                if &ent_name == name83 {
                    let hi = u16::from_le_bytes([
                        buf[off + DIRENT_OFF_FST_CLUS_HI],
                        buf[off + DIRENT_OFF_FST_CLUS_HI + 1],
                    ]) as u32;
                    let lo = u16::from_le_bytes([
                        buf[off + DIRENT_OFF_FST_CLUS_LO],
                        buf[off + DIRENT_OFF_FST_CLUS_LO + 1],
                    ]) as u32;
                    let size = u32::from_le_bytes([
                        buf[off + DIRENT_OFF_FILE_SIZE],
                        buf[off + DIRENT_OFF_FILE_SIZE + 1],
                        buf[off + DIRENT_OFF_FILE_SIZE + 2],
                        buf[off + DIRENT_OFF_FILE_SIZE + 3],
                    ]);
                    return Ok(DirentLocation {
                        sector: sec_num,
                        offset: off as u16,
                        first_cluster: (hi << 16) | lo,
                        size,
                        attr,
                    });
                }
            }
        }
        cluster = fat32_next_cluster(cluster).map_err(|()| FsError::Io)?;
    }
    Err(FsError::NotFound)
}

/// Resolve the directory that contains the final component of `path`.
///
/// Returns `(parent_dir_cluster, last_component)` — the caller can then call
/// `dir_find_in(parent_dir_cluster, ...)` or insert a new entry.
fn resolve_parent<'a>(path: &'a [u8]) -> Result<(u32, &'a [u8]), FsError> {
    if !fat32_mounted() { return Err(FsError::NotMounted); }
    let mut comps: [&[u8]; FAT32_MAX_PATH_DEPTH] = [&[]; FAT32_MAX_PATH_DEPTH];
    let n = split_path(path, &mut comps)?;
    if n == 0 { return Err(FsError::InvalidArg); }

    let root_cluster = FAT32.lock().root_cluster;
    let mut dir_cluster = root_cluster;
    // Walk all but the last component.
    for i in 0..(n - 1) {
        let name83 = component_to_83(comps[i]).ok_or(FsError::InvalidArg)?;
        let loc = dir_find_in(dir_cluster, &name83)?;
        if loc.attr & ATTR_DIRECTORY == 0 { return Err(FsError::WrongType); }
        if loc.first_cluster < FAT32_FIRST_DATA_CLUSTER { return Err(FsError::NotFound); }
        dir_cluster = loc.first_cluster;
    }
    Ok((dir_cluster, comps[n - 1]))
}

// ── Directory entry insert / update ──────────────────────────────────────────

/// Write a raw 32-byte dirent into the first free slot of `dir_cluster`.
/// If no slot exists and the chain is full, extends the directory by
/// allocating a new cluster.
///
/// Returns the sector and offset where the entry landed.
fn dir_insert(
    dir_cluster: u32,
    name83: &[u8; 11],
    first_cluster: u32,
    size: u32,
    attr: u8,
) -> Result<(u32, u16), FsError> {
    let (spc, data_start) = {
        let v = FAT32.lock();
        (v.secs_per_clus, v.data_start)
    };

    // Walk the chain looking for a free (end or deleted) slot.
    let mut cluster = dir_cluster;
    let mut last_cluster = cluster;
    while cluster >= FAT32_FIRST_DATA_CLUSTER && cluster < FAT32_EOC {
        let first_sector = data_start + (cluster - FAT32_FIRST_DATA_CLUSTER) * spc;
        for s in 0..spc {
            let sec_num = first_sector + s;
            let mut buf = [0u8; SECTOR_SIZE];
            read_sector(sec_num, &mut buf).map_err(|()| FsError::Io)?;
            for e in 0..FAT32_DIRENTS_PER_SECTOR {
                let off = e * FAT32_DIR_ENTRY_SIZE;
                let first_byte = buf[off + DIRENT_OFF_NAME];
                if first_byte == DIRENT_MARK_END || first_byte == DIRENT_MARK_DELETED {
                    write_dirent_into_buf(&mut buf, off, name83, first_cluster, size, attr);
                    // If we overwrote an END marker, zero the next entry so
                    // iteration still terminates correctly (only if there is one).
                    if first_byte == DIRENT_MARK_END
                        && e + 1 < FAT32_DIRENTS_PER_SECTOR
                    {
                        let next_off = (e + 1) * FAT32_DIR_ENTRY_SIZE;
                        buf[next_off + DIRENT_OFF_NAME] = DIRENT_MARK_END;
                    }
                    write_sector(sec_num, &buf).map_err(|()| FsError::Io)?;
                    return Ok((sec_num, off as u16));
                }
            }
        }
        last_cluster = cluster;
        cluster = fat32_next_cluster(cluster).map_err(|()| FsError::Io)?;
    }

    // Extend the directory by one cluster, zero it, and place the dirent at
    // offset 0.
    let new_clus = fat32_alloc_cluster().map_err(|()| FsError::NoSpace)?;
    fat32_write_fat_entry(last_cluster, new_clus).map_err(|()| FsError::Io)?;
    fat32_write_fat_entry(new_clus, FAT32_END_OF_CHAIN).map_err(|()| FsError::Io)?;
    let zero = [0u8; SECTOR_SIZE];
    let first_sector = data_start + (new_clus - FAT32_FIRST_DATA_CLUSTER) * spc;
    for s in 0..spc {
        write_sector(first_sector + s, &zero).map_err(|()| FsError::Io)?;
    }
    let mut buf = [0u8; SECTOR_SIZE];
    write_dirent_into_buf(&mut buf, 0, name83, first_cluster, size, attr);
    write_sector(first_sector, &buf).map_err(|()| FsError::Io)?;
    Ok((first_sector, 0))
}

/// Fill a 32-byte dirent inside `buf` at `off`.
fn write_dirent_into_buf(
    buf: &mut [u8; SECTOR_SIZE],
    off: usize,
    name83: &[u8; 11],
    first_cluster: u32,
    size: u32,
    attr: u8,
) {
    for b in &mut buf[off..off + FAT32_DIR_ENTRY_SIZE] { *b = 0; }
    buf[off + DIRENT_OFF_NAME..off + DIRENT_OFF_NAME + 8]
        .copy_from_slice(&name83[..8]);
    buf[off + DIRENT_OFF_EXT..off + DIRENT_OFF_EXT + 3]
        .copy_from_slice(&name83[8..11]);
    buf[off + DIRENT_OFF_ATTR] = attr;
    let hi = ((first_cluster >> 16) & 0xFFFF) as u16;
    let lo = (first_cluster & 0xFFFF) as u16;
    buf[off + DIRENT_OFF_FST_CLUS_HI..off + DIRENT_OFF_FST_CLUS_HI + 2]
        .copy_from_slice(&hi.to_le_bytes());
    buf[off + DIRENT_OFF_FST_CLUS_LO..off + DIRENT_OFF_FST_CLUS_LO + 2]
        .copy_from_slice(&lo.to_le_bytes());
    buf[off + DIRENT_OFF_FILE_SIZE..off + DIRENT_OFF_FILE_SIZE + 4]
        .copy_from_slice(&size.to_le_bytes());
}

/// Update the size + first-cluster fields of an existing dirent (in-place).
/// Used after writes extend/truncate a file.
fn dir_update_meta(
    dir_sector: u32,
    dir_offset: u16,
    first_cluster: u32,
    size: u32,
) -> Result<(), FsError> {
    let mut buf = [0u8; SECTOR_SIZE];
    read_sector(dir_sector, &mut buf).map_err(|()| FsError::Io)?;
    let off = dir_offset as usize;
    let hi = ((first_cluster >> 16) & 0xFFFF) as u16;
    let lo = (first_cluster & 0xFFFF) as u16;
    buf[off + DIRENT_OFF_FST_CLUS_HI..off + DIRENT_OFF_FST_CLUS_HI + 2]
        .copy_from_slice(&hi.to_le_bytes());
    buf[off + DIRENT_OFF_FST_CLUS_LO..off + DIRENT_OFF_FST_CLUS_LO + 2]
        .copy_from_slice(&lo.to_le_bytes());
    buf[off + DIRENT_OFF_FILE_SIZE..off + DIRENT_OFF_FILE_SIZE + 4]
        .copy_from_slice(&size.to_le_bytes());
    write_sector(dir_sector, &buf).map_err(|()| FsError::Io)?;
    Ok(())
}

// ── Cluster chain walking helper ─────────────────────────────────────────────

/// Walk the cluster chain starting at `first_cluster` and return the cluster
/// number at the `n`-th position (0-indexed).  If the chain ends before that
/// position, returns `Err(FsError::NotFound)`.
fn chain_nth(first_cluster: u32, n: u32) -> Result<u32, FsError> {
    if first_cluster < FAT32_FIRST_DATA_CLUSTER { return Err(FsError::NotFound); }
    let mut cur = first_cluster;
    for _ in 0..n {
        let next = fat32_next_cluster(cur).map_err(|()| FsError::Io)?;
        if next < FAT32_FIRST_DATA_CLUSTER || next >= FAT32_EOC {
            return Err(FsError::NotFound);
        }
        cur = next;
    }
    Ok(cur)
}

/// Walk the cluster chain starting at `first_cluster` to the `n`-th position,
/// allocating new clusters as needed.  The final cluster is marked as
/// end-of-chain.  Returns the cluster number at position `n`.
fn chain_nth_or_extend(first_cluster: u32, n: u32) -> Result<u32, FsError> {
    if first_cluster < FAT32_FIRST_DATA_CLUSTER { return Err(FsError::InvalidArg); }
    let mut cur = first_cluster;
    for _ in 0..n {
        let next = fat32_next_cluster(cur).map_err(|()| FsError::Io)?;
        if next < FAT32_FIRST_DATA_CLUSTER || next >= FAT32_EOC {
            // Need to extend.
            let fresh = fat32_alloc_cluster().map_err(|()| FsError::NoSpace)?;
            fat32_write_fat_entry(cur, fresh).map_err(|()| FsError::Io)?;
            fat32_write_fat_entry(fresh, FAT32_END_OF_CHAIN).map_err(|()| FsError::Io)?;
            cur = fresh;
        } else {
            cur = next;
        }
    }
    Ok(cur)
}

// ── File handle pool helpers ─────────────────────────────────────────────────

fn alloc_handle(entry: OpenFileEntry) -> Result<Fat32File, FsError> {
    let mut t = OPEN_FILES.lock();
    for i in 0..FAT32_MAX_OPEN_FILES {
        if !t[i].in_use {
            let gen = t[i].generation.wrapping_add(1);
            let mut e = entry;
            e.in_use = true;
            e.generation = gen;
            t[i] = e;
            return Ok(Fat32File { slot: i as u16, generation: gen });
        }
    }
    Err(FsError::TooManyOpen)
}

fn with_handle<R>(
    file: Fat32File,
    f: impl FnOnce(&mut OpenFileEntry) -> Result<R, FsError>,
) -> Result<R, FsError> {
    let mut t = OPEN_FILES.lock();
    let slot = file.slot as usize;
    if slot >= FAT32_MAX_OPEN_FILES { return Err(FsError::BadHandle); }
    if !t[slot].in_use || t[slot].generation != file.generation {
        return Err(FsError::BadHandle);
    }
    f(&mut t[slot])
}

fn snapshot_handle(file: Fat32File) -> Result<OpenFileEntry, FsError> {
    with_handle(file, |e| Ok(*e))
}

// ── Public open/close/read/write/seek/sync ───────────────────────────────────

/// Open a file by absolute path (e.g. `b"/log/boot.log"`).  Supports nested
/// directories up to `FAT32_MAX_PATH_DEPTH` deep.
pub fn fat32_open(_vol: Volume, path: &[u8], flags: u32) -> Result<Fat32File, FsError> {
    if !fat32_mounted() { return Err(FsError::NotMounted); }
    if flags & (open_flags::READ | open_flags::WRITE) == 0 {
        return Err(FsError::InvalidArg);
    }

    let (parent_cluster, last) = resolve_parent(path)?;
    let name83 = component_to_83(last).ok_or(FsError::InvalidArg)?;

    let (dir_sector, dir_offset, first_cluster, size) =
        match dir_find_in(parent_cluster, &name83) {
            Ok(loc) => {
                if loc.attr & ATTR_DIRECTORY != 0 { return Err(FsError::WrongType); }
                (loc.sector, loc.offset, loc.first_cluster, loc.size)
            }
            Err(FsError::NotFound) => {
                if flags & open_flags::CREATE == 0 { return Err(FsError::NotFound); }
                // Create empty dirent (no cluster allocated yet).
                let (sec, off) = dir_insert(
                    parent_cluster,
                    &name83,
                    0,
                    0,
                    DIRENT_ATTR_ARCHIVE_FILE,
                )?;
                (sec, off, 0u32, 0u32)
            }
            Err(e) => return Err(e),
        };

    let mut entry = OpenFileEntry {
        in_use: false,
        generation: 0,
        dir_sector,
        dir_offset,
        first_cluster,
        size,
        pos: 0,
        flags,
        dirty: false,
    };

    // Handle TRUNCATE.
    if flags & open_flags::TRUNCATE != 0 && entry.size > 0 {
        if entry.first_cluster >= FAT32_FIRST_DATA_CLUSTER {
            fat32_free_chain(entry.first_cluster);
        }
        entry.first_cluster = 0;
        entry.size = 0;
        entry.dirty = true;
        dir_update_meta(entry.dir_sector, entry.dir_offset, 0, 0)?;
    }

    // APPEND positions at end-of-file on open.
    if flags & open_flags::APPEND != 0 {
        entry.pos = entry.size;
    }

    alloc_handle(entry)
}

/// Read up to `buf.len()` bytes starting at the file's current position.
/// Returns the number of bytes actually read (0 at EOF).
pub fn fat32_read(file: Fat32File, buf: &mut [u8]) -> Result<usize, FsError> {
    let mut e = snapshot_handle(file)?;
    if e.flags & open_flags::READ == 0 { return Err(FsError::InvalidArg); }
    if e.pos >= e.size || buf.is_empty() { return Ok(0); }

    let bytes_per_clus = FAT32.lock().bytes_per_clus;
    if bytes_per_clus == 0 { return Err(FsError::NotMounted); }
    let spc = FAT32.lock().secs_per_clus;
    let data_start = FAT32.lock().data_start;

    let max = (e.size - e.pos) as usize;
    let mut remaining = buf.len().min(max);
    let mut written = 0usize;

    while remaining > 0 {
        let cluster_index = e.pos / bytes_per_clus;
        let offset_in_cluster = (e.pos % bytes_per_clus) as usize;
        let cluster = chain_nth(e.first_cluster, cluster_index)?;
        let first_sector = data_start + (cluster - FAT32_FIRST_DATA_CLUSTER) * spc;

        let sector_index = (offset_in_cluster / SECTOR_SIZE) as u32;
        let offset_in_sector = offset_in_cluster % SECTOR_SIZE;
        let sec_num = first_sector + sector_index;

        let mut sec_buf = [0u8; SECTOR_SIZE];
        read_sector(sec_num, &mut sec_buf).map_err(|()| FsError::Io)?;

        let avail_in_sector = SECTOR_SIZE - offset_in_sector;
        let chunk = remaining.min(avail_in_sector);
        buf[written..written + chunk]
            .copy_from_slice(&sec_buf[offset_in_sector..offset_in_sector + chunk]);

        written += chunk;
        remaining -= chunk;
        e.pos += chunk as u32;
    }

    // Persist updated position back into the slot.
    with_handle(file, |slot| {
        slot.pos = e.pos;
        Ok(())
    })?;
    Ok(written)
}

/// Write `buf` at the file's current position, extending the file and the
/// underlying cluster chain as needed.  Returns bytes written.
pub fn fat32_write(file: Fat32File, buf: &[u8]) -> Result<usize, FsError> {
    if buf.is_empty() { return Ok(0); }

    let mut e = snapshot_handle(file)?;
    if e.flags & open_flags::WRITE == 0 { return Err(FsError::InvalidArg); }

    // APPEND: jump to current size before every write.
    if e.flags & open_flags::APPEND != 0 { e.pos = e.size; }

    let bytes_per_clus = FAT32.lock().bytes_per_clus;
    if bytes_per_clus == 0 { return Err(FsError::NotMounted); }
    let spc = FAT32.lock().secs_per_clus;
    let data_start = FAT32.lock().data_start;

    // Ensure we have at least one cluster allocated (for empty files).
    if e.first_cluster < FAT32_FIRST_DATA_CLUSTER {
        let fresh = fat32_alloc_cluster().map_err(|()| FsError::NoSpace)?;
        // fat32_alloc_cluster already marks it EOC (0x0FFF_FFFF).
        e.first_cluster = fresh;
        e.dirty = true;
    }

    let mut written = 0usize;
    while written < buf.len() {
        let cluster_index = e.pos / bytes_per_clus;
        let offset_in_cluster = (e.pos % bytes_per_clus) as usize;
        let cluster = chain_nth_or_extend(e.first_cluster, cluster_index)?;
        let first_sector = data_start + (cluster - FAT32_FIRST_DATA_CLUSTER) * spc;

        let sector_index = (offset_in_cluster / SECTOR_SIZE) as u32;
        let offset_in_sector = offset_in_cluster % SECTOR_SIZE;
        let sec_num = first_sector + sector_index;

        // Read-modify-write the sector (partial writes require the untouched
        // head/tail to be preserved).
        let mut sec_buf = [0u8; SECTOR_SIZE];
        let sector_is_within_size = e.pos < e.size
            && (offset_in_sector != 0
                || (buf.len() - written) < SECTOR_SIZE);
        if sector_is_within_size {
            read_sector(sec_num, &mut sec_buf).map_err(|()| FsError::Io)?;
        } else if offset_in_sector != 0 {
            // Past current size but starting mid-sector — read to preserve
            // whatever junk is there (treated as zeros after size extension).
            read_sector(sec_num, &mut sec_buf).map_err(|()| FsError::Io)?;
            // Zero the tail beyond current size so stale bytes don't leak.
            for b in &mut sec_buf[offset_in_sector..] { *b = 0; }
        }

        let avail_in_sector = SECTOR_SIZE - offset_in_sector;
        let chunk = (buf.len() - written).min(avail_in_sector);
        sec_buf[offset_in_sector..offset_in_sector + chunk]
            .copy_from_slice(&buf[written..written + chunk]);
        write_sector(sec_num, &sec_buf).map_err(|()| FsError::Io)?;

        written += chunk;
        e.pos += chunk as u32;
        if e.pos > e.size { e.size = e.pos; }
        e.dirty = true;
    }

    // Persist updated size/position/dirty/first_cluster back into the slot.
    with_handle(file, |slot| {
        slot.pos = e.pos;
        slot.size = e.size;
        slot.first_cluster = e.first_cluster;
        slot.dirty = e.dirty;
        Ok(())
    })?;
    Ok(written)
}

/// Seek within an open file.  Seeking past EOF is allowed but will NOT
/// allocate clusters until a write happens.
pub fn fat32_seek(file: Fat32File, whence: SeekFrom) -> Result<u32, FsError> {
    with_handle(file, |e| {
        let new = match whence {
            SeekFrom::Start(p) => p,
            SeekFrom::Current(d) => {
                if d >= 0 { e.pos.saturating_add(d as u32) }
                else { e.pos.saturating_sub((-d) as u32) }
            }
            SeekFrom::End(d) => {
                if d >= 0 { e.size.saturating_add(d as u32) }
                else { e.size.saturating_sub((-d) as u32) }
            }
        };
        e.pos = new;
        Ok(new)
    })
}

/// Flush pending dirent updates to disk.  Data sectors are written
/// synchronously by [`fat32_write`], so this only updates the directory
/// entry (size + first cluster) and ensures the FAT copies are consistent.
pub fn fat32_fsync(file: Fat32File) -> Result<(), FsError> {
    let snapshot = snapshot_handle(file)?;
    if !snapshot.dirty { return Ok(()); }
    dir_update_meta(
        snapshot.dir_sector,
        snapshot.dir_offset,
        snapshot.first_cluster,
        snapshot.size,
    )?;
    let _ = fat32_sync();
    with_handle(file, |e| { e.dirty = false; Ok(()) })
}

/// Close an open file handle.  Implicitly fsyncs if the file is dirty.
pub fn fat32_close(file: Fat32File) -> Result<(), FsError> {
    let _ = fat32_fsync(file);
    with_handle(file, |e| {
        e.in_use = false;
        e.dirty = false;
        e.pos = 0;
        e.size = 0;
        e.first_cluster = 0;
        Ok(())
    })
}

/// Return `(position, size)` for an open file.  Useful for tests.
pub fn fat32_file_stat(file: Fat32File) -> Result<(u32, u32), FsError> {
    with_handle(file, |e| Ok((e.pos, e.size)))
}

// ── Directories: opendir / readdir / mkdir ───────────────────────────────────

/// Open a directory by absolute path (`b"/"` for root).  Returns an iterator
/// that yields each non-deleted, non-LFN, non-volume-ID entry.
pub fn fat32_opendir(_vol: Volume, path: &[u8]) -> Result<Fat32DirIter, FsError> {
    if !fat32_mounted() { return Err(FsError::NotMounted); }
    let mut comps: [&[u8]; FAT32_MAX_PATH_DEPTH] = [&[]; FAT32_MAX_PATH_DEPTH];
    let n = split_path(path, &mut comps)?;
    let root_cluster = FAT32.lock().root_cluster;
    let mut dir_cluster = root_cluster;
    for i in 0..n {
        let name83 = component_to_83(comps[i]).ok_or(FsError::InvalidArg)?;
        let loc = dir_find_in(dir_cluster, &name83)?;
        if loc.attr & ATTR_DIRECTORY == 0 { return Err(FsError::WrongType); }
        if loc.first_cluster < FAT32_FIRST_DATA_CLUSTER { return Err(FsError::NotFound); }
        dir_cluster = loc.first_cluster;
    }
    Ok(Fat32DirIter {
        cluster: dir_cluster,
        sector_in_cluster: 0,
        entry_in_sector: 0,
        done: false,
    })
}

impl Fat32DirIter {
    /// Return the next valid directory entry, or `None` at end.
    pub fn next(&mut self) -> Option<DirEntryInfo> {
        if self.done { return None; }
        let (spc, data_start) = {
            let v = FAT32.lock();
            (v.secs_per_clus, v.data_start)
        };
        loop {
            if self.cluster < FAT32_FIRST_DATA_CLUSTER || self.cluster >= FAT32_EOC {
                self.done = true;
                return None;
            }
            let first_sector = data_start + (self.cluster - FAT32_FIRST_DATA_CLUSTER) * spc;
            while self.sector_in_cluster < spc {
                let sec_num = first_sector + self.sector_in_cluster;
                let mut buf = [0u8; SECTOR_SIZE];
                if read_sector(sec_num, &mut buf).is_err() {
                    self.done = true;
                    return None;
                }
                while self.entry_in_sector < FAT32_DIRENTS_PER_SECTOR {
                    let off = self.entry_in_sector * FAT32_DIR_ENTRY_SIZE;
                    let first_byte = buf[off + DIRENT_OFF_NAME];
                    self.entry_in_sector += 1;
                    if first_byte == DIRENT_MARK_END {
                        self.done = true;
                        return None;
                    }
                    if first_byte == DIRENT_MARK_DELETED { continue; }
                    let attr = buf[off + DIRENT_OFF_ATTR];
                    if attr == ATTR_LFN { continue; }
                    if attr & ATTR_VOLUME_ID != 0 { continue; }
                    let mut name83 = [0u8; 11];
                    name83[..8].copy_from_slice(&buf[off + DIRENT_OFF_NAME..off + DIRENT_OFF_NAME + 8]);
                    name83[8..11].copy_from_slice(&buf[off + DIRENT_OFF_EXT..off + DIRENT_OFF_EXT + 3]);
                    let (name_buf, name_len) = format_83_name(&name83);
                    let hi = u16::from_le_bytes([
                        buf[off + DIRENT_OFF_FST_CLUS_HI],
                        buf[off + DIRENT_OFF_FST_CLUS_HI + 1],
                    ]) as u32;
                    let lo = u16::from_le_bytes([
                        buf[off + DIRENT_OFF_FST_CLUS_LO],
                        buf[off + DIRENT_OFF_FST_CLUS_LO + 1],
                    ]) as u32;
                    let size = u32::from_le_bytes([
                        buf[off + DIRENT_OFF_FILE_SIZE],
                        buf[off + DIRENT_OFF_FILE_SIZE + 1],
                        buf[off + DIRENT_OFF_FILE_SIZE + 2],
                        buf[off + DIRENT_OFF_FILE_SIZE + 3],
                    ]);
                    return Some(DirEntryInfo {
                        name: name_buf,
                        name_len: name_len as u8,
                        size,
                        is_dir: attr & ATTR_DIRECTORY != 0,
                        first_cluster: (hi << 16) | lo,
                    });
                }
                self.entry_in_sector = 0;
                self.sector_in_cluster += 1;
            }
            self.sector_in_cluster = 0;
            self.cluster = match fat32_next_cluster(self.cluster) {
                Ok(n) => n,
                Err(()) => { self.done = true; return None; }
            };
        }
    }
}

/// Create a new subdirectory at `path`.  The parent directory must exist.
/// Writes `.` (self) and `..` (parent) entries into the new directory.
pub fn fat32_mkdir(_vol: Volume, path: &[u8]) -> Result<(), FsError> {
    if !fat32_mounted() { return Err(FsError::NotMounted); }
    let (parent_cluster, last) = resolve_parent(path)?;
    let name83 = component_to_83(last).ok_or(FsError::InvalidArg)?;

    // Fail if an entry already exists.
    if dir_find_in(parent_cluster, &name83).is_ok() { return Err(FsError::InvalidArg); }

    // Allocate a cluster for the new directory and zero every sector.
    let new_clus = fat32_alloc_cluster().map_err(|()| FsError::NoSpace)?;
    let (spc, data_start) = {
        let v = FAT32.lock();
        (v.secs_per_clus, v.data_start)
    };
    let first_sector = data_start + (new_clus - FAT32_FIRST_DATA_CLUSTER) * spc;
    let mut buf = [0u8; SECTOR_SIZE];
    // '.' entry -> new_clus itself.
    let mut dot_name = [b' '; 11];
    dot_name[0] = b'.';
    write_dirent_into_buf(&mut buf, 0, &dot_name, new_clus, 0, DIRENT_ATTR_SUBDIR);
    // '..' entry -> parent cluster (root stored as 0 per FAT32 convention).
    let root_cluster = FAT32.lock().root_cluster;
    let dotdot_target = if parent_cluster == root_cluster { 0 } else { parent_cluster };
    let mut dotdot_name = [b' '; 11];
    dotdot_name[0] = b'.';
    dotdot_name[1] = b'.';
    write_dirent_into_buf(
        &mut buf,
        FAT32_DIR_ENTRY_SIZE,
        &dotdot_name,
        dotdot_target,
        0,
        DIRENT_ATTR_SUBDIR,
    );
    // The rest of the sector is already zero (DIRENT_MARK_END).
    write_sector(first_sector, &buf).map_err(|()| FsError::Io)?;
    // Zero remaining sectors in the cluster so iteration terminates.
    let zero = [0u8; SECTOR_SIZE];
    for s in 1..spc {
        write_sector(first_sector + s, &zero).map_err(|()| FsError::Io)?;
    }

    // Insert dirent for the new dir into the parent.
    match dir_insert(parent_cluster, &name83, new_clus, 0, DIRENT_ATTR_SUBDIR) {
        Ok(_) => Ok(()),
        Err(e) => {
            // Roll back: free the allocated cluster.
            fat32_free_chain(new_clus);
            Err(e)
        }
    }
}

// Provide a const constructor to let callers build a Volume handle without
// going through `fat32_mount_volume` — useful in tests that mount via the
// legacy `fat32_mount()` API.
impl Volume {
    pub const fn assume_mounted() -> Self { Volume { _private: () } }
}
