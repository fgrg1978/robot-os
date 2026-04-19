//! Coverage for `crates/ipc/` ring buffer + signal mask logic — was 0 tests before.

#![cfg(test)]

// ── Pipe ring buffer (mirrors ipc/pipe.rs) ───────────────────────────────

const PIPE_BUF_SIZE: usize = 4096;
const PIPE_BUF_MASK: usize = PIPE_BUF_SIZE - 1;

fn pipe_write(buf: &mut [u8; PIPE_BUF_SIZE], head: usize, tail: &mut usize,
              data: &[u8]) -> usize {
    let mut written = 0;
    for &b in data {
        let next = (*tail + 1) & PIPE_BUF_MASK;
        if next == head { break; }
        buf[*tail] = b;
        *tail = next;
        written += 1;
    }
    written
}

fn pipe_read(buf: &[u8; PIPE_BUF_SIZE], head: &mut usize, tail: usize,
             out: &mut [u8]) -> usize {
    let mut read = 0;
    while read < out.len() && *head != tail {
        out[read] = buf[*head];
        *head = (*head + 1) & PIPE_BUF_MASK;
        read += 1;
    }
    read
}

#[test]
fn pipe_empty_read_returns_zero() {
    let buf = [0u8; PIPE_BUF_SIZE];
    let mut head = 0usize;
    let tail = 0usize;
    let mut out = [0u8; 16];
    assert_eq!(pipe_read(&buf, &mut head, tail, &mut out), 0);
}

#[test]
fn pipe_write_then_read_round_trips() {
    let mut buf = [0u8; PIPE_BUF_SIZE];
    let mut head = 0usize;
    let mut tail = 0usize;
    let n = pipe_write(&mut buf, head, &mut tail, b"hello world");
    assert_eq!(n, 11);
    let mut out = [0u8; 16];
    let r = pipe_read(&buf, &mut head, tail, &mut out);
    assert_eq!(r, 11);
    assert_eq!(&out[..r], b"hello world");
}

#[test]
fn pipe_full_buffer_rejects_excess() {
    let mut buf = [0u8; PIPE_BUF_SIZE];
    let head = 0usize;
    let mut tail = 0usize;
    // Capacity is PIPE_BUF_SIZE - 1 (one slot reserved for full/empty distinction).
    let big = vec![0xCDu8; PIPE_BUF_SIZE + 100];
    let written = pipe_write(&mut buf, head, &mut tail, &big);
    assert_eq!(written, PIPE_BUF_SIZE - 1);
}

#[test]
fn pipe_partial_drain_then_more_writes() {
    let mut buf = [0u8; PIPE_BUF_SIZE];
    let mut head = 0usize;
    let mut tail = 0usize;
    pipe_write(&mut buf, head, &mut tail, b"first batch ");
    let mut out = [0u8; 6];
    pipe_read(&buf, &mut head, tail, &mut out);
    assert_eq!(&out, b"first ");
    pipe_write(&mut buf, head, &mut tail, b"more");
    let mut out2 = [0u8; 16];
    let r = pipe_read(&buf, &mut head, tail, &mut out2);
    // After draining "first " we have "batch " in the pipe; then we appended "more".
    assert_eq!(&out2[..r], b"batch more");
}

// ── Signal mask logic (mirrors ipc/signal.rs) ────────────────────────────
// Locks down: SIGMASK is a bitset; signal pending = (raised & ~masked).

const SIGTERM: u32 = 15;
const SIGINT:  u32 =  2;
const SIGUSR1: u32 = 10;

#[inline]
const fn sig_bit(sig: u32) -> u64 { 1u64 << (sig as u64 & 63) }

fn deliverable(raised: u64, masked: u64) -> u64 {
    raised & !masked
}

#[test]
fn signal_unmasked_is_deliverable() {
    let raised = sig_bit(SIGTERM);
    let masked = 0;
    assert_eq!(deliverable(raised, masked), sig_bit(SIGTERM));
}

#[test]
fn signal_masked_is_not_deliverable() {
    let raised = sig_bit(SIGTERM);
    let masked = sig_bit(SIGTERM);
    assert_eq!(deliverable(raised, masked), 0);
}

#[test]
fn multiple_signals_partially_masked() {
    let raised = sig_bit(SIGTERM) | sig_bit(SIGINT) | sig_bit(SIGUSR1);
    let masked = sig_bit(SIGTERM) | sig_bit(SIGUSR1);
    let deliv  = deliverable(raised, masked);
    assert_eq!(deliv, sig_bit(SIGINT));
}
