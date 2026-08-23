//! `phanes-config` — build-time Kconfig reader and Rust const emitter
//!
//! RFC-0026 Phase C2 implementation.
//!
//! # Usage (Phase C2 onwards)
//!
//! In `crates/limits/build.rs`:
//!
//! ```ignore
//! use phanes_config::{parse_config, emit_rust};
//! use std::collections::HashMap;
//!
//! fn main() {
//!     let cfg = parse_config(".config").expect("no .config — run `make defconfig-edge`");
//!     let mut out = String::new();
//!     emit_rust(&cfg, &mut out, "abcdef012345");  // 12-char SHA-256 prefix
//!     std::fs::write(&format!("{}/generated.rs", std::env::var("OUT_DIR").unwrap()), out).unwrap();
//! }
//! ```

use std::collections::HashMap;

/// Key→value map of all `CONFIG_*` entries from a `.config` file.
///
/// Keys are stored **without** the `CONFIG_` prefix (stripped on parse).
/// Values are unquoted strings (e.g. `"y"`, `"512"`, `"edge"`, `"10.0.2.2"`).
pub type ConfigMap = HashMap<String, String>;

/// Error type for configuration parsing failures.
#[derive(Debug)]
pub enum ConfigError {
    /// The `.config` file could not be opened or read.
    Io(std::io::Error),
    /// A line in the file does not conform to `CONFIG_KEY=value` format.
    ParseError { line: usize, text: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "IO error reading .config: {e}"),
            ConfigError::ParseError { line, text } => {
                write!(f, ".config parse error at line {line}: {text:?}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        ConfigError::Io(e)
    }
}

/// Parse a kconfiglib-generated `.config` file into a [`ConfigMap`].
///
/// Each non-comment, non-blank line must be either:
/// - `CONFIG_FOO=value`  — a set option.
/// - `# CONFIG_FOO is not set` — an unset bool option (stored as `"n"`).
///
/// Keys are stored **without** the `CONFIG_` prefix.
/// String values have surrounding double-quotes stripped.
/// Bool `y` options store `"y"`; unset bools store `"n"`.
///
/// # Errors
///
/// Returns [`ConfigError::Io`] if the file cannot be read, or
/// [`ConfigError::ParseError`] if an unexpected line format is found.
pub fn parse_config(path: &str) -> Result<ConfigMap, ConfigError> {
    use std::io::{BufRead, BufReader};

    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut cfg = ConfigMap::new();

    for (idx, line_result) in reader.lines().enumerate() {
        let line_num = idx + 1;
        let line = line_result?;
        let trimmed = line.trim();

        // Skip blank lines
        if trimmed.is_empty() {
            continue;
        }

        // Handle "# CONFIG_FOO is not set" — unset bool, store as "n"
        if let Some(rest) = trimmed.strip_prefix("# CONFIG_") {
            if let Some(key) = rest.strip_suffix(" is not set") {
                cfg.insert(key.to_string(), "n".to_string());
            }
            // Other comment lines (section headers, etc.) are silently skipped.
            continue;
        }

        // Skip other comment lines (e.g. `# Deployment Profile`)
        if trimmed.starts_with('#') {
            continue;
        }

        // Parse CONFIG_KEY=value
        if !trimmed.starts_with("CONFIG_") {
            return Err(ConfigError::ParseError {
                line: line_num,
                text: line.clone(),
            });
        }

        let body = &trimmed["CONFIG_".len()..];
        let eq_pos = body.find('=').ok_or_else(|| ConfigError::ParseError {
            line: line_num,
            text: line.clone(),
        })?;

        let key = &body[..eq_pos];
        let raw_val = &body[eq_pos + 1..];

        // Strip outer double-quotes from string values (kconfiglib format).
        let value = if raw_val.starts_with('"') && raw_val.ends_with('"') && raw_val.len() >= 2 {
            raw_val[1..raw_val.len() - 1].to_string()
        } else {
            raw_val.to_string()
        };

        cfg.insert(key.to_string(), value);
    }

    Ok(cfg)
}

// ---------------------------------------------------------------------------
// Type inference for Rust const emission
// ---------------------------------------------------------------------------

/// Determine the Rust integer type for a config key based on its name suffix.
///
/// Mapping (longest-suffix-first to avoid false matches):
/// - `_HZ`         → `u32`    (scheduler/timer frequencies)
/// - `_MS`         → `u64`    (millisecond durations)
/// - `_US`         → `u64`    (microsecond durations)
/// - `_TICKS`      → `u64`    (hardware timer ticks; can exceed u32)
/// - `_BYTES`      → `usize`  (buffer sizes)
/// - `_KB`         → `usize`  (sizes in KiB, before ×1024 expansion)
/// - `_MB`         → `usize`  (sizes in MiB, before ×1024×1024 expansion)
/// - `_SIZE`       → `usize`  (generic size)
/// - `_COUNT`      → `usize`  (table / slot counts)
/// - `_ATTEMPTS`   → `u32`    (retry counters)
/// - `_PROBES`     → `u32`    (keepalive probe count)
/// - `_MULT`       → `u32`    (multipliers)
/// - `_MM`         → `u32`    (millimetre measurements)
/// - `_FREQ`       → `u32`    (hardware frequencies)
/// - `_CPUS`       → `u32`    (CPU counts)
/// - default       → `usize`
fn infer_int_type(key: &str) -> &'static str {
    // Longest-suffix-first to avoid partial matches.
    let suffixes: &[(&str, &str)] = &[
        ("_ATTEMPTS", "u32"),
        ("_PROBES", "u32"),
        ("_MULT", "u32"),
        ("_TICKS", "u64"),
        ("_BYTES", "usize"),
        ("_SIZE", "usize"),
        ("_COUNT", "usize"),
        ("_FREQ", "u32"),
        ("_CPUS", "u32"),
        ("_HZ", "u32"),
        ("_MS", "u64"),
        ("_US", "u64"),
        ("_KB", "usize"),
        ("_MB", "usize"),
        ("_MM", "u32"),
    ];
    for (suffix, ty) in suffixes {
        if key.ends_with(suffix) {
            return ty;
        }
    }
    "usize"
}

/// Options whose values must be multiplied before emission.
///
/// Returns `(rust_name, scale_factor)`.  The scale is applied to the raw
/// integer value before writing the const literal.
///
/// Handles the Kconfig.limits pattern where `KERNEL_HEAP_SIZE` is stored
/// in KiB but consumers want bytes.
fn byte_expanded_key(key: &str) -> Option<(&'static str, u64)> {
    match key {
        // KERNEL_HEAP_SIZE is declared in KiB in Kconfig.limits
        "KERNEL_HEAP_SIZE"    => Some(("KERNEL_HEAP_SIZE_BYTES",    1024)),
        // Explicit KB suffixed stack sizes
        "USER_STACK_SIZE_KB"        => Some(("USER_STACK_SIZE_BYTES",        1024)),
        "KERNEL_STACK_SIZE_KB"      => Some(("KERNEL_STACK_SIZE_BYTES",      1024)),
        "INTERRUPT_STACK_SIZE_KB"   => Some(("INTERRUPT_STACK_SIZE_BYTES",   1024)),
        // OTA image size
        "OTA_MAX_IMAGE_SIZE_MB"     => Some(("OTA_MAX_IMAGE_SIZE_BYTES", 1024 * 1024)),
        _ => None,
    }
}

/// Emit a Rust source file containing `pub const` declarations for every
/// option in `cfg`.
///
/// - Int options → `pub const FOO: <type> = <value>;`
/// - Bool options → `pub const FOO: bool = true/false;`
/// - Hex options (0x…) → `pub const FOO: usize = 0x…;`
/// - String options → `pub const FOO: &str = "<value>";`
/// - Byte-expanded keys (KERNEL_HEAP_SIZE, stack sizes, OTA size) emit
///   an *additional* `_BYTES` constant with the expanded value, alongside
///   the raw constant.
///
/// The `config_sha12` parameter is the first 12 hex characters of the
/// SHA-256 of the `.config` file, embedded in the header comment so
/// audit logs can match a binary to a config snapshot.
pub fn emit_rust(cfg: &ConfigMap, out: &mut String, config_sha12: &str) {
    out.push_str("// GENERATED by crates/phanes-config from .config — DO NOT EDIT.\n");
    out.push_str(&format!(
        "// Config SHA-256 prefix: {config_sha12} — use `sha256sum .config` to verify.\n"
    ));
    out.push_str("//\n");
    out.push_str("// Each Kconfig integer/bool/string option becomes a `pub const`.\n");
    out.push_str("// Change values via `make menuconfig` or `make defconfig-<profile>`.\n");
    out.push('\n');
    // Note: inner attributes (#![...]) are not valid inside include!().
    // Consumers suppress dead_code at the use site or in lib.rs.


    // Collect and sort keys for deterministic output.
    let mut keys: Vec<&String> = cfg.keys().collect();
    keys.sort();

    for key in keys {
        let val = &cfg[key];
        emit_single_const(key, val, out);
    }
}

/// Emit a single `pub const` (or a pair for byte-expanded keys).
fn emit_single_const(key: &str, val: &str, out: &mut String) {
    // Hex values (0x… prefix)
    if val.starts_with("0x") || val.starts_with("0X") {
        // Parse as u64 to avoid overflow; emit as usize (addresses/bases).
        match u64::from_str_radix(&val[2..], 16) {
            Ok(_n) => {
                // Emit raw hex literal for readability
                out.push_str(&format!("pub const {key}: usize = {val};\n"));
            }
            Err(_) => {
                out.push_str(&format!("// WARN: could not parse hex value for {key}: {val}\n"));
            }
        }
        return;
    }

    // Bool values: "y" → true, "n" → false
    if val == "y" || val == "n" {
        let bool_val = val == "y";
        out.push_str(&format!("pub const {key}: bool = {bool_val};\n"));
        return;
    }

    // Integer values (pure decimal)
    if let Ok(n) = val.parse::<u64>() {
        let rust_type = infer_int_type(key);
        out.push_str(&format!("pub const {key}: {rust_type} = {n};\n"));

        // Emit additional byte-expanded const if this key needs it.
        if let Some((expanded_name, scale)) = byte_expanded_key(key) {
            let expanded_val = n.saturating_mul(scale);
            out.push_str(&format!("pub const {expanded_name}: usize = {expanded_val};\n"));
        }
        return;
    }

    // String values (everything else)
    // Escape any remaining double-quotes or backslashes in the value.
    let escaped = val.replace('\\', "\\\\").replace('"', "\\\"");
    out.push_str(&format!("pub const {key}: &str = \"{escaped}\";\n"));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn parse_str(content: &str) -> ConfigMap {
        use std::time::{SystemTime, UNIX_EPOCH};
        let dir = std::env::temp_dir();
        // Use a unique file name per call to avoid races between parallel tests.
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        // Include thread ID for uniqueness when multiple tests run simultaneously.
        let tid = std::thread::current().id();
        let path = dir.join(format!("phanes_config_test_{ts}_{tid:?}.config"));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        drop(f);
        let result = parse_config(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        result
    }

    #[test]
    fn test_parse_simple_int() {
        let cfg = parse_str("CONFIG_MAX_TASKS=64\n");
        assert_eq!(cfg["MAX_TASKS"], "64");
    }

    #[test]
    fn test_parse_bool_y() {
        let cfg = parse_str("CONFIG_ARCH_RISCV64=y\n");
        assert_eq!(cfg["ARCH_RISCV64"], "y");
    }

    #[test]
    fn test_parse_bool_not_set() {
        let cfg = parse_str("# CONFIG_ARCH_AARCH64 is not set\n");
        assert_eq!(cfg["ARCH_AARCH64"], "n");
    }

    #[test]
    fn test_parse_string_unquoted() {
        let cfg = parse_str("CONFIG_BRAIN_SERVER_IP_DEFAULT=\"10.0.2.2\"\n");
        // Quotes must be stripped
        assert_eq!(cfg["BRAIN_SERVER_IP_DEFAULT"], "10.0.2.2");
    }

    #[test]
    fn test_parse_blank_and_comment_lines() {
        let content = "\n# Architecture\nCONFIG_ARCH_RISCV64=y\n\n# end of Architecture\n";
        let cfg = parse_str(content);
        assert_eq!(cfg["ARCH_RISCV64"], "y");
        // Comment header lines should not appear as keys
        assert!(!cfg.contains_key("Architecture"));
    }

    #[test]
    fn test_emit_int() {
        let mut cfg = ConfigMap::new();
        cfg.insert("MAX_TASKS".to_string(), "64".to_string());
        let mut out = String::new();
        emit_rust(&cfg, &mut out, "000000000000");
        assert!(out.contains("pub const MAX_TASKS: usize = 64;"));
    }

    #[test]
    fn test_emit_bool_true() {
        let mut cfg = ConfigMap::new();
        cfg.insert("ARCH_RISCV64".to_string(), "y".to_string());
        let mut out = String::new();
        emit_rust(&cfg, &mut out, "000000000000");
        assert!(out.contains("pub const ARCH_RISCV64: bool = true;"));
    }

    #[test]
    fn test_emit_bool_false() {
        let mut cfg = ConfigMap::new();
        cfg.insert("ARCH_AARCH64".to_string(), "n".to_string());
        let mut out = String::new();
        emit_rust(&cfg, &mut out, "000000000000");
        assert!(out.contains("pub const ARCH_AARCH64: bool = false;"));
    }

    #[test]
    fn test_emit_string() {
        let mut cfg = ConfigMap::new();
        cfg.insert("BRAIN_SERVER_IP_DEFAULT".to_string(), "10.0.2.2".to_string());
        let mut out = String::new();
        emit_rust(&cfg, &mut out, "000000000000");
        assert!(out.contains(r#"pub const BRAIN_SERVER_IP_DEFAULT: &str = "10.0.2.2";"#));
    }

    #[test]
    fn test_emit_hex() {
        let mut cfg = ConfigMap::new();
        cfg.insert("UART_BASE".to_string(), "0x10000000".to_string());
        let mut out = String::new();
        emit_rust(&cfg, &mut out, "000000000000");
        assert!(out.contains("pub const UART_BASE: usize = 0x10000000;"));
    }

    #[test]
    fn test_emit_kernel_heap_size_expanded() {
        let mut cfg = ConfigMap::new();
        cfg.insert("KERNEL_HEAP_SIZE".to_string(), "32768".to_string()); // 32 MiB in KiB
        let mut out = String::new();
        emit_rust(&cfg, &mut out, "000000000000");
        assert!(out.contains("pub const KERNEL_HEAP_SIZE: usize = 32768;"));
        assert!(out.contains(&format!(
            "pub const KERNEL_HEAP_SIZE_BYTES: usize = {};",
            32768_u64 * 1024
        )));
    }

    #[test]
    fn test_emit_user_stack_size_expanded() {
        let mut cfg = ConfigMap::new();
        cfg.insert("USER_STACK_SIZE_KB".to_string(), "16".to_string());
        let mut out = String::new();
        emit_rust(&cfg, &mut out, "000000000000");
        assert!(out.contains("pub const USER_STACK_SIZE_BYTES: usize = 16384;"));
    }

    #[test]
    fn test_emit_ota_max_image_size_expanded() {
        let mut cfg = ConfigMap::new();
        cfg.insert("OTA_MAX_IMAGE_SIZE_MB".to_string(), "8".to_string());
        let mut out = String::new();
        emit_rust(&cfg, &mut out, "000000000000");
        assert!(out.contains(&format!(
            "pub const OTA_MAX_IMAGE_SIZE_BYTES: usize = {};",
            8_u64 * 1024 * 1024
        )));
    }

    #[test]
    fn test_infer_int_type() {
        assert_eq!(infer_int_type("SCHED_HZ"), "u32");
        assert_eq!(infer_int_type("RTO_INITIAL_MS"), "u64");
        assert_eq!(infer_int_type("WCET_BOUND_PID_US"), "u64");
        assert_eq!(infer_int_type("KEEPALIVE_INTERVAL_TICKS"), "u64");
        assert_eq!(infer_int_type("TCP_BUF_SIZE"), "usize");
        assert_eq!(infer_int_type("KERNEL_HEAP_SIZE"), "usize");
        assert_eq!(infer_int_type("OTA_SLOT_COUNT"), "usize");
        assert_eq!(infer_int_type("MAX_TASKS"), "usize");
    }
}
