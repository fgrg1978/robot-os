//! Flattened Device Tree (FDT) parser for bare-metal RISC-V.
//!
//! No alloc, no heap, no external dependencies.  Parses the DTB blob
//! that firmware (OpenSBI / U-Boot) passes in `a1` at kernel entry and
//! returns a [`DtbInfo`] with the fields the kernel needs to
//! self-configure at boot.
//!
//! All multi-byte integers in FDT are **big-endian**.

#![no_std]

// ---------------------------------------------------------------
// FDT structure-block token constants
// ---------------------------------------------------------------
const FDT_MAGIC: u32 = 0xd00d_feed;
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

// ---------------------------------------------------------------
// Public types
// ---------------------------------------------------------------

/// Information extracted from a parsed FDT blob.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct DtbInfo {
    /// Physical base address of main memory (from /memory reg).
    pub mem_base: usize,
    /// Size of main memory in bytes.
    pub mem_size: usize,
    /// Timer frequency in Hz (timebase-frequency).
    pub timer_freq: u64,
    /// Base address of the first UART / serial device.
    pub uart_base: usize,
    /// Base address of the PLIC (interrupt-controller).
    pub plic_base: usize,
    /// Number of cpu@N children under /cpus.
    pub num_cpus: usize,
    /// Compatible string from the root node (NUL-terminated, truncated).
    pub compatible: [u8; 64],
}

impl DtbInfo {
    const fn zeroed() -> Self {
        Self {
            mem_base: 0,
            mem_size: 0,
            timer_freq: 0,
            uart_base: 0,
            plic_base: 0,
            num_cpus: 0,
            compatible: [0u8; 64],
        }
    }
}

// ---------------------------------------------------------------
// FDT header (40 bytes)
// ---------------------------------------------------------------

struct FdtHeader {
    magic: u32,
    totalsize: u32,
    off_dt_struct: u32,
    off_dt_strings: u32,
    _off_mem_rsvmap: u32,
    version: u32,
    _last_comp_version: u32,
    _boot_cpuid_phys: u32,
    size_dt_strings: u32,
    _size_dt_struct: u32,
}

// ---------------------------------------------------------------
// Safe byte-level readers (no alignment requirements)
// ---------------------------------------------------------------

/// Read a big-endian u32 from `base + offset`.
///
/// # Safety
/// `base` must point to a valid DTB blob and `offset + 4` must be
/// within `totalsize`.
#[inline]
unsafe fn read_be32(base: *const u8, offset: usize) -> u32 {
    let p = base.add(offset);
    let b = [
        core::ptr::read(p),
        core::ptr::read(p.add(1)),
        core::ptr::read(p.add(2)),
        core::ptr::read(p.add(3)),
    ];
    u32::from_be_bytes(b)
}

/// Read a big-endian u64 from `base + offset`.
#[inline]
unsafe fn read_be64(base: *const u8, offset: usize) -> u64 {
    let hi = read_be32(base, offset) as u64;
    let lo = read_be32(base, offset + 4) as u64;
    (hi << 32) | lo
}

/// Read a NUL-terminated C string starting at `base + offset`.
/// Returns the byte length **excluding** the terminator.
#[inline]
unsafe fn strlen(base: *const u8, offset: usize) -> usize {
    let mut len: usize = 0;
    while core::ptr::read(base.add(offset + len)) != 0 {
        len += 1;
    }
    len
}

/// Compare a NUL-terminated C string at `base + offset` with `needle`.
/// Returns `true` if they are equal up to the NUL.
#[inline]
unsafe fn streq(base: *const u8, offset: usize, needle: &[u8]) -> bool {
    for (i, &ch) in needle.iter().enumerate() {
        if core::ptr::read(base.add(offset + i)) != ch {
            return false;
        }
    }
    core::ptr::read(base.add(offset + needle.len())) == 0
}

/// Check whether the C string at `base + offset` starts with `prefix`.
#[inline]
unsafe fn starts_with(base: *const u8, offset: usize, prefix: &[u8]) -> bool {
    for (i, &ch) in prefix.iter().enumerate() {
        if core::ptr::read(base.add(offset + i)) != ch {
            return false;
        }
    }
    true
}

/// Align `v` up to a 4-byte boundary.
#[inline]
const fn align4(v: usize) -> usize {
    (v + 3) & !3
}

// ---------------------------------------------------------------
// Header parser
// ---------------------------------------------------------------

unsafe fn parse_header(base: *const u8) -> Option<FdtHeader> {
    let magic = read_be32(base, 0);
    if magic != FDT_MAGIC {
        return None;
    }
    Some(FdtHeader {
        magic,
        totalsize: read_be32(base, 4),
        off_dt_struct: read_be32(base, 8),
        off_dt_strings: read_be32(base, 12),
        _off_mem_rsvmap: read_be32(base, 16),
        version: read_be32(base, 20),
        _last_comp_version: read_be32(base, 24),
        _boot_cpuid_phys: read_be32(base, 28),
        size_dt_strings: read_be32(base, 32),
        _size_dt_struct: read_be32(base, 36),
    })
}

// ---------------------------------------------------------------
// Structure-block walker
// ---------------------------------------------------------------

/// Internal state kept while walking the structure block.
struct Walker {
    /// Pointer to the beginning of the DTB blob.
    base: *const u8,
    /// Offset of the structure block relative to `base`.
    struct_off: usize,
    /// Offset of the strings block relative to `base`.
    strings_off: usize,
    /// Upper bound for the strings block.
    strings_end: usize,
    /// Total size of the DTB blob (safety bound).
    totalsize: usize,
    /// Current byte offset inside the structure block (relative to
    /// `struct_off`).
    cursor: usize,
    /// Nesting depth.
    depth: usize,
    /// Result being accumulated.
    info: DtbInfo,

    // -- contextual flags while walking ---
    /// True when we are inside a /memory node at depth 1.
    in_memory: bool,
    /// True when we are inside /cpus at depth 1.
    in_cpus: bool,
    /// True when we are inside /cpus/cpu@* at depth 2.
    in_cpu_child: bool,
    /// True when we are inside a node whose name starts with
    /// "serial" or "uart" at any depth.
    in_uart: bool,
    /// True when we are inside an interrupt-controller node.
    in_intc: bool,
    /// #address-cells in the current context (default 2).
    address_cells: u32,
    /// #size-cells in the current context (default 1).
    size_cells: u32,

    /// Depth at which the current "interesting" node was entered,
    /// so we know when we leave it.
    uart_depth: usize,
    intc_depth: usize,
}

impl Walker {
    /// Read the next big-endian u32 token from the structure block and
    /// advance the cursor by 4.
    #[inline]
    unsafe fn next_u32(&mut self) -> u32 {
        let off = self.struct_off + self.cursor;
        self.cursor += 4;
        read_be32(self.base, off)
    }

    /// Resolve a property name from the strings block.
    #[inline]
    unsafe fn prop_name_eq(&self, nameoff: u32, needle: &[u8]) -> bool {
        let off = self.strings_off + nameoff as usize;
        if off >= self.strings_end {
            return false;
        }
        streq(self.base, off, needle)
    }

    /// Walk the entire structure block, populating `self.info`.
    unsafe fn walk(&mut self) {
        loop {
            // Safety bound — don't walk past the blob.
            if self.struct_off + self.cursor + 4 > self.totalsize {
                break;
            }

            let token = self.next_u32();

            match token {
                FDT_BEGIN_NODE => self.handle_begin_node(),
                FDT_END_NODE => self.handle_end_node(),
                FDT_PROP => self.handle_prop(),
                FDT_NOP => { /* skip */ }
                FDT_END => break,
                _ => break, // malformed
            }
        }
    }

    unsafe fn handle_begin_node(&mut self) {
        let name_off = self.struct_off + self.cursor;
        let name_len = strlen(self.base, name_off);
        // Advance past name + NUL, then align to 4.
        self.cursor += align4(name_len + 1);
        self.depth += 1;

        // Detect which node we entered.
        if self.depth == 1 {
            // Root-level children.
            if streq(self.base, name_off, b"memory")
                || starts_with(self.base, name_off, b"memory@")
            {
                self.in_memory = true;
            } else if streq(self.base, name_off, b"cpus") {
                self.in_cpus = true;
            }
        }

        if self.depth == 2 && self.in_cpus {
            if starts_with(self.base, name_off, b"cpu@") {
                self.in_cpu_child = true;
                self.info.num_cpus += 1;
            }
        }

        // UART / serial can appear at any depth.
        if !self.in_uart {
            if starts_with(self.base, name_off, b"serial")
                || starts_with(self.base, name_off, b"uart")
            {
                self.in_uart = true;
                self.uart_depth = self.depth;
            }
        }

        // Interrupt controller.
        if !self.in_intc {
            if starts_with(self.base, name_off, b"interrupt-controller")
                || starts_with(self.base, name_off, b"plic")
            {
                self.in_intc = true;
                self.intc_depth = self.depth;
            }
        }
    }

    unsafe fn handle_end_node(&mut self) {
        if self.depth == 1 {
            self.in_memory = false;
            self.in_cpus = false;
        }
        if self.depth == 2 {
            self.in_cpu_child = false;
        }
        if self.in_uart && self.depth == self.uart_depth {
            self.in_uart = false;
        }
        if self.in_intc && self.depth == self.intc_depth {
            self.in_intc = false;
        }
        if self.depth > 0 {
            self.depth -= 1;
        }
    }

    unsafe fn handle_prop(&mut self) {
        let prop_len = self.next_u32() as usize;
        let nameoff = self.next_u32();
        let data_off = self.struct_off + self.cursor;
        // Advance past data, aligned to 4.
        self.cursor += align4(prop_len);

        // --- root compatible ---
        if self.depth == 1
            && !self.in_memory
            && !self.in_cpus
            && self.prop_name_eq(nameoff, b"compatible")
        {
            let copy_len = if prop_len < 64 { prop_len } else { 63 };
            for i in 0..copy_len {
                self.info.compatible[i] = core::ptr::read(self.base.add(data_off + i));
            }
            // Ensure NUL termination.
            self.info.compatible[copy_len] = 0;
            return;
        }

        // --- #address-cells / #size-cells (used for reg parsing) ---
        if self.prop_name_eq(nameoff, b"#address-cells") && prop_len == 4 {
            self.address_cells = read_be32(self.base, data_off);
        }
        if self.prop_name_eq(nameoff, b"#size-cells") && prop_len == 4 {
            self.size_cells = read_be32(self.base, data_off);
        }

        // --- memory reg ---
        if self.in_memory && self.prop_name_eq(nameoff, b"reg") {
            self.parse_mem_reg(data_off, prop_len);
            return;
        }

        // --- timebase-frequency (in /cpus or /cpus/cpu@N) ---
        if (self.in_cpus || self.in_cpu_child)
            && self.prop_name_eq(nameoff, b"timebase-frequency")
        {
            if prop_len == 4 {
                self.info.timer_freq = read_be32(self.base, data_off) as u64;
            } else if prop_len == 8 {
                self.info.timer_freq = read_be64(self.base, data_off);
            }
            return;
        }

        // --- UART reg (take only the first one found) ---
        if self.in_uart && self.info.uart_base == 0 && self.prop_name_eq(nameoff, b"reg") {
            self.info.uart_base = self.read_addr(data_off, prop_len);
            return;
        }

        // --- PLIC reg (take only the first one found) ---
        if self.in_intc && self.info.plic_base == 0 && self.prop_name_eq(nameoff, b"reg") {
            self.info.plic_base = self.read_addr(data_off, prop_len);
            return;
        }
    }

    /// Parse a "reg" property for /memory.  Handles #address-cells = 1 or 2,
    /// #size-cells = 1 or 2.
    unsafe fn parse_mem_reg(&mut self, data_off: usize, prop_len: usize) {
        let ac = self.address_cells;
        let sc = self.size_cells;
        let entry_bytes = (ac as usize + sc as usize) * 4;
        if prop_len < entry_bytes || entry_bytes == 0 {
            return;
        }
        let base = if ac == 2 {
            read_be64(self.base, data_off) as usize
        } else {
            read_be32(self.base, data_off) as usize
        };
        let size_off = data_off + ac as usize * 4;
        let size = if sc == 2 {
            read_be64(self.base, size_off) as usize
        } else {
            read_be32(self.base, size_off) as usize
        };
        self.info.mem_base = base;
        self.info.mem_size = size;
    }

    /// Read the first address from a "reg" property, respecting
    /// #address-cells.
    unsafe fn read_addr(&self, data_off: usize, prop_len: usize) -> usize {
        let ac = self.address_cells;
        let min = ac as usize * 4;
        if prop_len < min || min == 0 {
            return 0;
        }
        if ac >= 2 {
            read_be64(self.base, data_off) as usize
        } else {
            read_be32(self.base, data_off) as usize
        }
    }
}

// ---------------------------------------------------------------
// Public API
// ---------------------------------------------------------------

/// Parse the Flattened Device Tree blob at `ptr` and return a [`DtbInfo`]
/// with the extracted hardware description.
///
/// Returns `None` if the magic number does not match or the header is
/// obviously invalid.
///
/// # Safety
/// `ptr` must point to a valid, complete FDT blob that remains readable
/// for its entire `totalsize`.  The pointer does **not** need to be
/// aligned.
pub unsafe fn dtb_parse(ptr: *const u8) -> Option<DtbInfo> {
    if ptr.is_null() {
        return None;
    }

    let hdr = parse_header(ptr)?;
    if hdr.magic != FDT_MAGIC {
        return None;
    }
    // Minimal sanity: version >= 16 (the oldest version we support).
    if hdr.version < 16 {
        return None;
    }
    if hdr.totalsize < 40 {
        return None;
    }

    let mut walker = Walker {
        base: ptr,
        struct_off: hdr.off_dt_struct as usize,
        strings_off: hdr.off_dt_strings as usize,
        strings_end: hdr.off_dt_strings as usize + hdr.size_dt_strings as usize,
        totalsize: hdr.totalsize as usize,
        cursor: 0,
        depth: 0,
        info: DtbInfo::zeroed(),
        in_memory: false,
        in_cpus: false,
        in_cpu_child: false,
        in_uart: false,
        in_intc: false,
        address_cells: 2,
        size_cells: 1,
        uart_depth: 0,
        intc_depth: 0,
    };

    walker.walk();
    Some(walker.info)
}

/// Return the compatible string from a [`DtbInfo`] as a byte slice
/// (up to the first NUL or end of buffer).
pub fn dtb_compatible_str(info: &DtbInfo) -> &[u8] {
    let mut len = 0usize;
    while len < info.compatible.len() && info.compatible[len] != 0 {
        len += 1;
    }
    &info.compatible[..len]
}
