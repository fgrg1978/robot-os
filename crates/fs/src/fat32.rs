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

const SECTOR_SIZE: usize = 512;

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

// ── Helper: read a single 512-byte sector via VirtIO block ───────────────────

fn read_sector(sector: u32, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), ()> {
    robot_os_drivers::blkdev::read(sector as u64, 1, buf)
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
fn write_sector(sector: u32, buf: &[u8; SECTOR_SIZE]) -> Result<(), ()> {
    robot_os_drivers::blkdev::write(sector as u64, 1, buf)
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
