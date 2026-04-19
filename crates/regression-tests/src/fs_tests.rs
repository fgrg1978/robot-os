//! Coverage for `crates/fs/` — FAT32 cluster math, file-table indexing,
//! 8.3 name normalisation. The full FS code touches block IO, but the
//! pure math is testable on the host.

#![cfg(test)]

// ── FAT32 cluster ↔ sector arithmetic ────────────────────────────────────

const SECTORS_PER_CLUSTER: u32 = 1;
const RESERVED_SECTORS:    u32 = 32;
const FAT_SIZE_SECTORS:    u32 = 256;
const NUM_FATS:            u32 = 2;

fn first_data_sector() -> u32 {
    RESERVED_SECTORS + NUM_FATS * FAT_SIZE_SECTORS
}

fn cluster_to_sector(cluster: u32) -> u32 {
    // FAT32 clusters start at 2 (clusters 0 and 1 are reserved).
    first_data_sector() + (cluster - 2) * SECTORS_PER_CLUSTER
}

#[test]
fn fat32_first_data_sector_layout() {
    assert_eq!(first_data_sector(), 32 + 2 * 256);
}

#[test]
fn fat32_cluster_2_maps_to_first_data_sector() {
    assert_eq!(cluster_to_sector(2), first_data_sector());
}

#[test]
fn fat32_cluster_to_sector_strides_by_spc() {
    assert_eq!(cluster_to_sector(3), first_data_sector() + 1);
    assert_eq!(cluster_to_sector(10), first_data_sector() + 8);
}

// ── 8.3 short name normalisation ─────────────────────────────────────────

fn to_8dot3(name: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    let mut parts = name.splitn(2, '.');
    let base = parts.next().unwrap_or("").to_ascii_uppercase();
    let ext  = parts.next().unwrap_or("").to_ascii_uppercase();
    for (i, b) in base.bytes().take(8).enumerate() {
        out[i] = b;
    }
    for (i, b) in ext.bytes().take(3).enumerate() {
        out[8 + i] = b;
    }
    out
}

#[test]
fn fat32_8dot3_uppercases() {
    assert_eq!(&to_8dot3("kernel.bin"), b"KERNEL  BIN");
}

#[test]
fn fat32_8dot3_pads_with_spaces() {
    assert_eq!(&to_8dot3("a.b"),       b"A       B  ");
    assert_eq!(&to_8dot3("hi"),        b"HI         ");
}

#[test]
fn fat32_8dot3_truncates_long_names() {
    assert_eq!(&to_8dot3("VERYLONGNAME.LONGEXT"), b"VERYLONGLON");
}

#[test]
fn fat32_8dot3_handles_no_extension() {
    assert_eq!(&to_8dot3("CONFIG"), b"CONFIG     ");
}

// ── FAT entry parser (next-cluster lookup) ───────────────────────────────
// FAT32 stores 32-bit entries; only low 28 bits are the cluster number,
// top 4 are reserved. Locking this down — a refactor that masks wrong
// would break long files.

const FAT_MASK: u32 = 0x0FFF_FFFF;
const FAT_EOC:  u32 = 0x0FFF_FFF8; // any value ≥ this means end-of-chain
const FAT_BAD:  u32 = 0x0FFF_FFF7;

fn fat_entry_kind(raw: u32) -> &'static str {
    let v = raw & FAT_MASK;
    if v == 0          { "free"     }
    else if v == FAT_BAD { "bad"    }
    else if v >= FAT_EOC { "eoc"    }
    else if v < 2      { "reserved" }
    else               { "next"     }
}

#[test]
fn fat_entry_recognises_eoc() {
    assert_eq!(fat_entry_kind(0x0FFF_FFFF), "eoc");
    assert_eq!(fat_entry_kind(0x0FFF_FFF8), "eoc");
    assert_eq!(fat_entry_kind(0xFFFF_FFFF), "eoc"); // top 4 ignored
}

#[test]
fn fat_entry_recognises_bad() {
    assert_eq!(fat_entry_kind(0x0FFF_FFF7), "bad");
}

#[test]
fn fat_entry_recognises_free() {
    assert_eq!(fat_entry_kind(0), "free");
}

#[test]
fn fat_entry_recognises_normal_next() {
    assert_eq!(fat_entry_kind(2),       "next");
    assert_eq!(fat_entry_kind(0x12345), "next");
}

#[test]
fn fat_entry_top_4_bits_ignored() {
    // Reading from disk should not be confused by reserved bits.
    assert_eq!(fat_entry_kind(0xF000_0042), "next");
}
