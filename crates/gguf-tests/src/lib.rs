//! Host-side tests for the GGUF parser.
//!
//! Pulls `crates/ml/src/gguf.rs` directly via `#[path]` so we test
//! the same source the kernel uses, without dragging in
//! `robot_os_arch` (which the full `ml` crate depends on and which
//! is riscv-only).

#[path = "../../ml/src/gguf.rs"]
pub mod gguf;

#[cfg(test)]
mod tests {
    use super::gguf::{GgmlType, GgufFile, MAX_TENSORS};

    // ── GGUF KV type constants — duplicated here so the tests
    // don't depend on private items of gguf.rs.
    const GGUF_TYPE_UINT32: u32 = 4;
    const GGUF_TYPE_STRING: u32 = 8;

    // ── Builder helpers ────────────────────────────────────────

    /// 24-byte header: magic + version + n_tensors + n_kv.
    fn header(version: u32, n_tensors: u64, n_kv: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(24);
        out.extend_from_slice(b"GGUF");
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&n_tensors.to_le_bytes());
        out.extend_from_slice(&n_kv.to_le_bytes());
        out
    }

    fn push_string(buf: &mut Vec<u8>, s: &[u8]) {
        buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
        buf.extend_from_slice(s);
    }

    /// Push one tensor-info entry: name + n_dims + dims[] + type + offset.
    fn push_tensor_info(
        buf: &mut Vec<u8>, name: &[u8], dims: &[u64],
        ggml_type: u32, data_offset: u64,
    ) {
        push_string(buf, name);
        buf.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for &d in dims {
            buf.extend_from_slice(&d.to_le_bytes());
        }
        buf.extend_from_slice(&ggml_type.to_le_bytes());
        buf.extend_from_slice(&data_offset.to_le_bytes());
    }

    /// Pad `buf` to next 32-byte boundary (v3 data alignment).
    fn pad_to_32(buf: &mut Vec<u8>) {
        let pad = (32 - (buf.len() % 32)) % 32;
        buf.extend(core::iter::repeat(0u8).take(pad));
    }

    // ── Rejection cases ────────────────────────────────────────

    #[test]
    fn rejects_short_buffer() {
        // Header is 24 bytes; anything less must fail.
        assert!(GgufFile::parse(&[]).is_none());
        assert!(GgufFile::parse(b"GGUF").is_none());
        assert!(GgufFile::parse(&[0u8; 23]).is_none());
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut buf = header(3, 0, 0);
        buf[0] = b'X';
        assert!(GgufFile::parse(&buf).is_none());
    }

    #[test]
    fn rejects_version_0() {
        let buf = header(0, 0, 0);
        assert!(GgufFile::parse(&buf).is_none());
    }

    #[test]
    fn rejects_version_4_and_above() {
        for v in [4u32, 5, 99, u32::MAX] {
            let buf = header(v, 0, 0);
            assert!(GgufFile::parse(&buf).is_none(),
                "version {} must be rejected (impl supports 1..=3)", v);
        }
    }

    #[test]
    fn accepts_versions_1_to_3() {
        for v in [1u32, 2, 3] {
            let buf = header(v, 0, 0);
            assert!(GgufFile::parse(&buf).is_some(),
                "version {} must parse (impl supports 1..=3)", v);
        }
    }

    #[test]
    fn rejects_too_many_tensors() {
        // MAX_TENSORS is 32; declare 33.
        let buf = header(3, (MAX_TENSORS + 1) as u64, 0);
        assert!(GgufFile::parse(&buf).is_none());
    }

    // ── Happy-path parses ──────────────────────────────────────

    #[test]
    fn empty_blob_parses() {
        let buf = header(3, 0, 0);
        let f = GgufFile::parse(&buf).unwrap();
        assert_eq!(f.n_tensors, 0);
    }

    #[test]
    fn parses_one_tensor_metadata() {
        let mut buf = header(3, 1, 0);
        push_tensor_info(&mut buf, b"weights.0", &[4, 2], GgmlType::F32 as u32, 0);
        // Tensor data: 4*2 f32 = 32 bytes, aligned to 32-byte boundary.
        pad_to_32(&mut buf);
        buf.extend(core::iter::repeat(0u8).take(32));

        let f = GgufFile::parse(&buf).unwrap();
        assert_eq!(f.n_tensors, 1);
        let info = f.tensor_info(b"weights.0").unwrap();
        assert_eq!(info.name_bytes(), b"weights.0");
        assert_eq!(info.n_dims, 2);
        assert_eq!(info.dims[0], 4);
        assert_eq!(info.dims[1], 2);
        assert_eq!(info.n_elements(), 8);
    }

    #[test]
    fn tensor_data_returns_correct_slice() {
        let mut buf = header(3, 1, 0);
        push_tensor_info(&mut buf, b"w", &[4], GgmlType::F32 as u32, 0);
        pad_to_32(&mut buf);
        // Write 4 f32s (16 bytes) of known data.
        let pattern: [u8; 16] = [
            1, 2, 3, 4,  5, 6, 7, 8,
            9,10,11,12, 13,14,15,16,
        ];
        buf.extend_from_slice(&pattern);

        let f = GgufFile::parse(&buf).unwrap();
        let (data, ty, n) = f.tensor_data(b"w").unwrap();
        assert_eq!(n, 4);
        assert_eq!(ty as u32, GgmlType::F32 as u32);
        assert_eq!(data, &pattern[..]);
    }

    #[test]
    fn tensor_data_missing_returns_none() {
        let mut buf = header(3, 1, 0);
        push_tensor_info(&mut buf, b"present", &[1], GgmlType::F32 as u32, 0);
        pad_to_32(&mut buf);
        buf.extend(core::iter::repeat(0u8).take(4)); // 1 × f32
        let f = GgufFile::parse(&buf).unwrap();
        assert!(f.tensor_data(b"absent").is_none());
    }

    // ── Helper-method sanity ───────────────────────────────────

    #[test]
    fn ggml_type_byte_size_f32() {
        assert_eq!(GgmlType::F32.byte_size(0),   0);
        assert_eq!(GgmlType::F32.byte_size(1),   4);
        assert_eq!(GgmlType::F32.byte_size(100), 400);
    }

    #[test]
    fn ggml_type_byte_size_f16() {
        // F16 = 2 bytes per element.
        assert_eq!(GgmlType::F16.byte_size(10), 20);
    }

    #[test]
    fn ggml_type_byte_size_q4_0_quantised_block() {
        // Q4_0 is block-quantised: 32 values pack into 18 bytes
        // (2-byte f16 scale + 16 bytes of nibbles).  Asking for
        // 32 elements ⇒ 1 block ⇒ 18 bytes.
        assert_eq!(GgmlType::Q4_0.byte_size(32), 18);
        // 64 elements ⇒ 2 blocks ⇒ 36 bytes.
        assert_eq!(GgmlType::Q4_0.byte_size(64), 36);
        // 33 elements: rounds up to 2 blocks (can't represent a
        // partial block in this format).
        assert_eq!(GgmlType::Q4_0.byte_size(33), 36);
    }

    #[test]
    fn tensor_data_truncated_returns_none() {
        let mut buf = header(3, 1, 0);
        push_tensor_info(&mut buf, b"big", &[100], GgmlType::F32 as u32, 0);
        pad_to_32(&mut buf);
        // Promise 100 f32s (400 bytes) but only provide 16.
        buf.extend(core::iter::repeat(0u8).take(16));
        let f = GgufFile::parse(&buf).unwrap();
        assert!(f.tensor_data(b"big").is_none(),
            "tensor_data must reject when declared size walks past EOF");
    }

    // ── Skipping KV metadata ───────────────────────────────────

    #[test]
    fn skips_a_simple_kv_string_then_parses_zero_tensors() {
        let mut buf = header(3, 0, 1);
        // KV: key (string) + type (u32) + value
        push_string(&mut buf, b"general.architecture");
        buf.extend_from_slice(&GGUF_TYPE_STRING.to_le_bytes());
        push_string(&mut buf, b"llama");

        let f = GgufFile::parse(&buf).unwrap();
        assert_eq!(f.n_tensors, 0);
    }

    #[test]
    fn skips_a_simple_kv_uint32_then_parses_zero_tensors() {
        let mut buf = header(3, 0, 1);
        push_string(&mut buf, b"some.scalar");
        buf.extend_from_slice(&GGUF_TYPE_UINT32.to_le_bytes());
        buf.extend_from_slice(&123u32.to_le_bytes());

        let f = GgufFile::parse(&buf).unwrap();
        assert_eq!(f.n_tensors, 0);
    }
}
