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

/// Size of the FDT header in bytes (Devicetree Specification v0.4 §5.2).
/// Any blob claiming a `totalsize` below this cannot even contain its own
/// header, so it is rejected outright.
const FDT_HEADER_SIZE: usize = 40;

/// Hard upper bound on the blob size we are willing to walk.
///
/// WHY THIS EXISTS — do not remove as "redundant":
/// `totalsize` is an attacker/firmware-controlled u32 read from the very
/// first bytes of the blob, and it is the *only* bound `walk()` has. With
/// it unclamped, a blob claiming `totalsize = 0xFFFF_FFFF` licenses the
/// walker to march ~4 GiB past the end of physical RAM. That happens at
/// `kernel/src/main.rs` before the trap handler is useful, and with
/// `panic = "abort"` a load access fault there is a full board reset with
/// no diagnostics.
///
/// 4 MiB is deliberately generous: real DTBs are ~10-100 KiB (QEMU virt
/// with 8 harts is under 8 KiB), and even the largest server-class device
/// trees stay well under 1 MiB. Anything above this is malformed, not big.
///
/// Consequence of tripping this bound: `dtb_parse` returns `None` and the
/// kernel falls back to its hardcoded memory map / CPU count. That is a
/// degraded boot, never a fatal one — which is exactly why the bound is
/// safe to enforce strictly.
const MAX_DTB_SIZE: usize = 4 * 1024 * 1024;

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

// ---------------------------------------------------------------
// Bounded C-string helpers
//
// EVERY one of these takes an exclusive `end` offset and refuses to read
// at or past it. WHY — do not "simplify" the bound away:
// FDT strings (node names in the structure block, property names in the
// strings block) are length-prefixed by *nothing*; they are terminated by
// a NUL that a malformed or truncated blob is under no obligation to
// provide. The blob comes from firmware via the raw `a1` register, so an
// unbounded scan walks off the end of the DTB and, moments later, off the
// end of physical RAM — a load access fault before the trap handler is
// usable, i.e. an unrecoverable board reset under `panic = "abort"`.
//
// The offsets are all derived from untrusted u32 header fields, so the
// address arithmetic uses checked adds too: `overflow-checks = true` turns
// a wrapping `offset + len` into a panic, which is the same board reset by
// a different route.
// ---------------------------------------------------------------

/// Read a NUL-terminated C string starting at `base + offset`, scanning no
/// further than `end` (exclusive, relative to `base`).
///
/// Returns the byte length **excluding** the terminator, or `None` if no
/// NUL was found before `end` — i.e. the blob is truncated/malformed and
/// the caller must abandon the walk rather than guess.
#[inline]
unsafe fn strlen_bounded(base: *const u8, offset: usize, end: usize) -> Option<usize> {
    let mut len: usize = 0;
    loop {
        let at = offset.checked_add(len)?;
        if at >= end {
            // Ran to the end of the blob without a terminator.
            return None;
        }
        if core::ptr::read(base.add(at)) == 0 {
            return Some(len);
        }
        len += 1;
    }
}

/// Compare a NUL-terminated C string at `base + offset` with `needle`,
/// reading nothing at or past `end` (exclusive, relative to `base`).
/// Returns `true` if they are equal up to the NUL.
///
/// Note the `+ 1`: this reads `needle.len()` bytes *plus* the terminator,
/// so the whole `needle.len() + 1` window must be inside the blob before a
/// single byte is touched. Checking only the first byte (as the caller
/// used to) leaves a name near the end of the blob reading past it.
#[inline]
unsafe fn streq(base: *const u8, offset: usize, end: usize, needle: &[u8]) -> bool {
    match offset.checked_add(needle.len()).and_then(|v| v.checked_add(1)) {
        Some(window_end) if window_end <= end => {}
        // Not enough blob left to hold `needle` + NUL: it cannot match, and
        // reading to find out would go out of bounds.
        _ => return false,
    }
    for (i, &ch) in needle.iter().enumerate() {
        if core::ptr::read(base.add(offset + i)) != ch {
            return false;
        }
    }
    core::ptr::read(base.add(offset + needle.len())) == 0
}

/// Check whether the C string at `base + offset` starts with `prefix`,
/// reading nothing at or past `end` (exclusive, relative to `base`).
///
/// Deliberately does **not** require a NUL after the prefix — callers use
/// it for genuine prefix matches like `"memory@"` against `"memory@80000000"`.
/// Only `prefix.len()` bytes need to be in bounds.
#[inline]
unsafe fn starts_with(base: *const u8, offset: usize, end: usize, prefix: &[u8]) -> bool {
    match offset.checked_add(prefix.len()) {
        Some(window_end) if window_end <= end => {}
        _ => return false,
    }
    for (i, &ch) in prefix.iter().enumerate() {
        if core::ptr::read(base.add(offset + i)) != ch {
            return false;
        }
    }
    true
}

/// Align `v` up to a 4-byte boundary, or `None` on overflow.
///
/// The input is an untrusted `prop_len`/name length straight out of the
/// blob; `(v + 3)` on a near-`usize::MAX` value panics under
/// `overflow-checks = true`, so the add is checked and the caller aborts
/// the walk instead.
#[inline]
fn align4_checked(v: usize) -> Option<usize> {
    v.checked_add(3).map(|x| x & !3)
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
    /// #address-cells in the current context (default 2 at root).
    /// Stack is indexed by `depth`; index 0 is the implicit pre-root default.
    /// `/cpus` typically declares `#address-cells=1, #size-cells=0`, which
    /// must NOT bleed into a sibling `/memory` reg parse — hence per-scope
    /// tracking.
    address_cells: u32,
    /// #size-cells in the current context (default 1 at root).
    size_cells: u32,
    /// Saved (address_cells, size_cells) per nesting depth, restored on
    /// FDT_END_NODE. Up to 8 levels deep is plenty for any realistic FDT.
    cells_stack: [(u32, u32); 8],

    /// Depth at which the current "interesting" node was entered,
    /// so we know when we leave it.
    uart_depth: usize,
    intc_depth: usize,
}

impl Walker {
    /// Read the next big-endian u32 token from the structure block and
    /// advance the cursor by 4.
    ///
    /// # Caller obligation — this method performs NO bounds check
    /// The caller must already have proved, with checked arithmetic, that
    /// `struct_off + cursor + 4 <= totalsize`. `walk()` does this before
    /// reading each token and `handle_prop()` does it for the 8 bytes of
    /// property header it consumes.
    ///
    /// Both the read and the raw `+` below depend on that: an unproved
    /// call reads outside the blob, and a wrapping `struct_off + cursor`
    /// panics under `overflow-checks = true` — either way a board reset,
    /// since this runs before the trap handler is useful. If you add a new
    /// token handler, you owe it that check.
    #[inline]
    unsafe fn next_u32(&mut self) -> u32 {
        let off = self.struct_off + self.cursor;
        self.cursor += 4;
        read_be32(self.base, off)
    }

    /// Resolve a property name from the strings block.
    ///
    /// `nameoff` is an untrusted u32 from the blob, so the add is checked;
    /// `strings_end` was clamped to `totalsize` in `dtb_parse`, and `streq`
    /// re-checks the full `needle.len() + 1` window against it. The old
    /// code guarded only the *first* byte, which let a property name sitting
    /// at the tail of the strings block read past the end of the blob.
    #[inline]
    unsafe fn prop_name_eq(&self, nameoff: u32, needle: &[u8]) -> bool {
        let off = match self.strings_off.checked_add(nameoff as usize) {
            Some(v) => v,
            None => return false,
        };
        if off >= self.strings_end {
            return false;
        }
        streq(self.base, off, self.strings_end, needle)
    }

    /// Walk the entire structure block, populating `self.info`.
    ///
    /// Handlers return `false` to abort the walk when the blob turns out to
    /// be truncated or malformed. Returning a value (rather than setting a
    /// flag) means the compiler forces every call site to deal with it — a
    /// missed abort here is an out-of-bounds read, not a wrong answer.
    unsafe fn walk(&mut self) {
        loop {
            // Safety bound — the 4-byte token itself must lie entirely
            // inside the blob. Checked adds because `struct_off` and
            // `cursor` both derive from untrusted blob contents and a wrap
            // would panic (= board reset) under `overflow-checks = true`.
            let token_end = match self
                .struct_off
                .checked_add(self.cursor)
                .and_then(|v| v.checked_add(4))
            {
                Some(v) => v,
                None => break,
            };
            if token_end > self.totalsize {
                break;
            }

            let token = self.next_u32();

            let keep_walking = match token {
                FDT_BEGIN_NODE => self.handle_begin_node(),
                // Asymmetry is deliberate: handle_end_node only pops
                // bookkeeping state and reads no blob memory, so it has no
                // failure mode to report.
                FDT_END_NODE => {
                    self.handle_end_node();
                    true
                }
                FDT_PROP => self.handle_prop(),
                FDT_NOP => true, /* skip */
                FDT_END => false,
                _ => false, // malformed
            };
            if !keep_walking {
                break;
            }
        }
    }

    /// Returns `false` if the node name is unterminated inside the blob, in
    /// which case the walk must stop.
    unsafe fn handle_begin_node(&mut self) -> bool {
        let name_off = match self.struct_off.checked_add(self.cursor) {
            Some(v) => v,
            None => return false,
        };
        // walk() only proved the 4-byte *token* is inside the blob; nothing
        // says a NUL follows the name. A blob whose last token is
        // FDT_BEGIN_NODE with an unterminated name would otherwise scan RAM
        // until it happened to hit a zero byte. Bound the scan at
        // `totalsize` — node names live in the structure block, so the blob
        // bound applies here, NOT `strings_end`.
        let name_len = match strlen_bounded(self.base, name_off, self.totalsize) {
            Some(l) => l,
            None => return false, // truncated blob — abandon the walk.
        };
        // Advance past name + NUL, then align to 4.
        let step = match name_len.checked_add(1).and_then(align4_checked) {
            Some(s) => s,
            None => return false,
        };
        self.cursor = match self.cursor.checked_add(step) {
            Some(c) => c,
            None => return false,
        };
        self.depth += 1;

        // Push the parent's (address_cells, size_cells) so any override
        // in this child node can be popped on FDT_END_NODE without leaking
        // into siblings (e.g. /cpus declares 1/0, /memory must still see 2/2).
        if self.depth < self.cells_stack.len() {
            self.cells_stack[self.depth] = (self.address_cells, self.size_cells);
        }

        // Detect which node we entered.
        // NOTE: walk() starts at depth=0; entering the FDT_BEGIN_NODE for the
        // (anonymous) root takes depth → 1. Root's direct children are
        // therefore at depth 2 (not 1, as an earlier version assumed —
        // that mistake silently zeroed `mem_base`, `num_cpus`, `timer_freq`
        // because /memory and /cpus were never recognised).
        // All of the name comparisons below are bounded by `totalsize`: the
        // name lives in the structure block and `strlen_bounded` above has
        // already proved its NUL sits inside the blob, so a needle longer
        // than the name simply mismatches instead of reading past the end.
        let end = self.totalsize;

        if self.depth == 2 {
            // Root-level children.
            if streq(self.base, name_off, end, b"memory")
                || starts_with(self.base, name_off, end, b"memory@")
            {
                self.in_memory = true;
            } else if streq(self.base, name_off, end, b"cpus") {
                self.in_cpus = true;
            }
        }

        if self.depth == 3 && self.in_cpus {
            if starts_with(self.base, name_off, end, b"cpu@") {
                self.in_cpu_child = true;
                self.info.num_cpus += 1;
            }
        }

        // UART / serial can appear at any depth.
        if !self.in_uart {
            if starts_with(self.base, name_off, end, b"serial")
                || starts_with(self.base, name_off, end, b"uart")
            {
                self.in_uart = true;
                self.uart_depth = self.depth;
            }
        }

        // Interrupt controller.
        if !self.in_intc {
            if starts_with(self.base, name_off, end, b"interrupt-controller")
                || starts_with(self.base, name_off, end, b"plic")
            {
                self.in_intc = true;
                self.intc_depth = self.depth;
            }
        }

        true
    }

    unsafe fn handle_end_node(&mut self) {
        // Mirrors the depth correction in handle_begin_node: root children
        // live at depth 2, /cpus/cpu@N at depth 3.
        if self.depth == 2 {
            self.in_memory = false;
            self.in_cpus = false;
        }
        if self.depth == 3 {
            self.in_cpu_child = false;
        }
        if self.in_uart && self.depth == self.uart_depth {
            self.in_uart = false;
        }
        if self.in_intc && self.depth == self.intc_depth {
            self.in_intc = false;
        }
        // Restore parent's (address_cells, size_cells) — critical so a
        // /cpus override of 1/0 doesn't leak into the sibling /memory parse.
        if self.depth > 0 && self.depth < self.cells_stack.len() {
            let (ac, sc) = self.cells_stack[self.depth];
            self.address_cells = ac;
            self.size_cells    = sc;
        }
        if self.depth > 0 {
            self.depth -= 1;
        }
    }

    /// Returns `false` if the property header or payload runs past the end
    /// of the blob, in which case the walk must stop.
    unsafe fn handle_prop(&mut self) -> bool {
        // walk() validated only the 4-byte FDT_PROP token. The property
        // header is two MORE big-endian u32 (len, nameoff) — 8 bytes that
        // used to be read with no bound at all, so a blob whose final token
        // was FDT_PROP read 8 bytes past the end regardless of how well the
        // header offsets checked out.
        let hdr_off = match self.struct_off.checked_add(self.cursor) {
            Some(v) => v,
            None => return false,
        };
        match hdr_off.checked_add(8) {
            Some(e) if e <= self.totalsize => {}
            _ => return false,
        }

        let prop_len = self.next_u32() as usize;
        let nameoff = self.next_u32();
        let data_off = hdr_off + 8; // == struct_off + cursor, already bounded

        // `prop_len` is an untrusted u32: bound the whole payload ONCE here
        // so every reader below (the `compatible` copy, the cells reads,
        // parse_mem_reg, read_addr) is operating inside the blob by
        // construction. Without this, `#address-cells` alone would read 4
        // bytes at an entirely unchecked `data_off`.
        let data_end = match data_off.checked_add(prop_len) {
            Some(v) => v,
            None => return false,
        };
        if data_end > self.totalsize {
            return false;
        }

        // Advance past data, aligned to 4.
        let step = match align4_checked(prop_len) {
            Some(s) => s,
            None => return false,
        };
        self.cursor = match self.cursor.checked_add(step) {
            Some(c) => c,
            None => return false,
        };

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
            return true;
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
            return true;
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
            return true;
        }

        // --- UART reg (take only the first one found) ---
        if self.in_uart && self.info.uart_base == 0 && self.prop_name_eq(nameoff, b"reg") {
            self.info.uart_base = self.read_addr(data_off, prop_len);
            return true;
        }

        // --- PLIC reg (take only the first one found) ---
        if self.in_intc && self.info.plic_base == 0 && self.prop_name_eq(nameoff, b"reg") {
            self.info.plic_base = self.read_addr(data_off, prop_len);
            return true;
        }

        true
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
///
/// **At least [`FDT_HEADER_SIZE`] bytes at `ptr` must be readable.** This
/// is the caller's guarantee and the one precondition this function cannot
/// check: `totalsize` — the bound every other read is validated against —
/// lives *inside* the header, so the header must be read before anything
/// is known about the blob's extent. Everything after that point is
/// defensive against a hostile or truncated blob; this first 40 bytes is
/// not, and cannot be.
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

    // ---- Blob-extent validation -------------------------------------
    // Everything below is the *only* thing standing between a firmware-
    // supplied u32 and a walker that dereferences it. These are not
    // redundant with the per-read bounds inside the walker: the walker's
    // bounds are all expressed relative to `totalsize`, `strings_off` and
    // `strings_end`, so if those three are nonsense the per-read checks
    // faithfully permit nonsense.
    //
    // Concretely, the bug this closes: with off_dt_strings = 0xFFFF_F000
    // and size_dt_strings = 0x1000, `strings_end` became 0x1_0000_0000,
    // `prop_name_eq`'s "off < strings_end" guard passed, and the first
    // FDT_PROP token resolved its name ~4 GiB past the blob — outside
    // physical RAM on every target board, so a load access fault during
    // early boot with no working trap handler.

    let totalsize = hdr.totalsize as usize;
    // A blob smaller than its own header cannot be coherent.
    if totalsize < FDT_HEADER_SIZE {
        return None;
    }
    // See MAX_DTB_SIZE: clamps how far walk() may ever march.
    if totalsize > MAX_DTB_SIZE {
        return None;
    }

    // The structure block must start inside the blob; walk() bounds the
    // cursor relative to it, so an out-of-blob `struct_off` would make
    // every subsequent bound meaningless.
    let struct_off = hdr.off_dt_struct as usize;
    if struct_off >= totalsize {
        return None;
    }

    // The strings block must lie wholly inside the blob. `<=` (not `<`) is
    // correct and deliberate: a blob whose strings block runs exactly to
    // the last byte — the normal layout emitted by dtc — has
    // off_dt_strings + size_dt_strings == totalsize.
    let strings_off = hdr.off_dt_strings as usize;
    let strings_end = match strings_off.checked_add(hdr.size_dt_strings as usize) {
        Some(v) => v,
        // Two u32 cannot overflow a 64-bit usize, but this crate also
        // builds for 32-bit RISC-V targets where they trivially can — and
        // `overflow-checks = true` turns that into a panic, i.e. a board
        // reset. Check it rather than rely on the pointer width.
        None => return None,
    };
    if strings_end > totalsize {
        return None;
    }

    let mut walker = Walker {
        base: ptr,
        struct_off,
        strings_off,
        strings_end,
        totalsize,
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
        cells_stack: [(2, 1); 8],
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
