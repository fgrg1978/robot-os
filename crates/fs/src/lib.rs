#![no_std]

extern crate alloc;

pub mod vfs;
pub mod fat32;
pub mod tmpfs;
pub mod procfs;

pub use fat32::{
    fat32_mount, fat32_lookup_root, fat32_read_chain, fat32_mounted, fat32_ls_root,
    fat32_write_file, fat32_unlink_root, fat32_unlink_path,
    fat32_alloc_cluster, fat32_free_chain, fat32_sync,
    fat32_locks_available,
    // File-level API (phase AS).
    fat32_mount_volume, fat32_unmount,
    fat32_open, fat32_read, fat32_write, fat32_seek, fat32_fsync, fat32_close,
    fat32_file_stat,
    fat32_opendir, fat32_mkdir,
    Volume, Fat32File, Fat32DirIter, DirEntryInfo,
    FsError, SeekFrom, open_flags,
    FAT32_MAX_OPEN_FILES,
};

pub use tmpfs::{
    tmpfs_write, tmpfs_read, tmpfs_size, tmpfs_unlink, tmpfs_ls, tmpfs_stats,
    TmpfsError, TMPFS_MAX_FILES, TMPFS_MAX_BYTES, TMPFS_NAME_LEN,
};

pub use procfs::{
    procfs_init, procfs_register, procfs_read, procfs_ls, procfs_count,
    ProcNs, PROCFS_MAX_ENTRIES,
};

pub use vfs::{
    FdTable, FileDesc, Inode, DentryEntry,
    MAX_FILES, MAX_FDS, MAX_FILENAME, MAX_PATH,
    INODE_FILE, INODE_DIR, INODE_DEVICE,
    PERM_READ, PERM_WRITE, PERM_EXEC,
    O_RDONLY, O_WRONLY, O_RDWR, O_CREAT, O_TRUNC, O_APPEND,
    SEEK_SET, SEEK_CUR, SEEK_END,
    FS_TYPE_FAT32,
    NO_IDX,
    cstr_to_bytes,
    init, inode_alloc, inode_free, inode_resize,
    dir_add_entry, dir_lookup, dir_remove_entry, dir_list, dir_entry_at,
    path_lookup, path_parent,
    vfs_mount,
    fd_table_init, fd_alloc, fd_free, fd_get, fd_dup, fd_dup2,
    vfs_open, vfs_close, vfs_read, vfs_write, vfs_lseek,
    vfs_fs_lock_available,
};
