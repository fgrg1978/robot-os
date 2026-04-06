//! Synchronous RPC — IPC_CALL / IPC_REPLY (F00.5).
//!
//! A client sends a message to a server channel and blocks until the server
//! replies. This enables request/response patterns between userspace drivers.
//!
//! Flow:
//!   1. Client: IPC_CALL(server_ch, msg) → message sent to channel, client blocks
//!   2. Server: CHAN_READ(server_ch) → reads message
//!   3. Server: IPC_REPLY(caller_tid, reply) → reply stored, client woken
//!   4. Client: wakes up, retrieves reply data

use core::sync::atomic::{AtomicBool, Ordering};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of concurrent pending RPCs.
pub const MAX_PENDING_RPCS: usize = 16;

/// Maximum RPC message/reply size in bytes.
pub const RPC_MSG_MAX_LEN: usize = 64;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A pending RPC call waiting for a reply.
pub struct RpcPending {
    /// Task ID of the caller (blocked).
    pub caller_tid: u32,
    /// Channel the call was sent to (for correlation/debugging).
    pub server_channel: u32,
    /// Reply buffer (kernel-side copy).
    pub reply_buf: [u8; RPC_MSG_MAX_LEN],
    /// Reply length (filled by server via IPC_REPLY).
    pub reply_len: u32,
    /// Whether this slot is active (waiting for reply).
    pub active: bool,
    /// Whether the reply has been written (server called IPC_REPLY).
    pub done: AtomicBool,
}

impl RpcPending {
    pub const fn empty() -> Self {
        Self {
            caller_tid: 0,
            server_channel: 0,
            reply_buf: [0u8; RPC_MSG_MAX_LEN],
            reply_len: 0,
            active: false,
            done: AtomicBool::new(false),
        }
    }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

static mut RPC_PENDING: [RpcPending; MAX_PENDING_RPCS] = {
    const EMPTY: RpcPending = RpcPending::empty();
    [EMPTY; MAX_PENDING_RPCS]
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Register a pending RPC call. Returns rpc_id or None if no free slots.
/// Called by kernel when processing SYS_IPC_CALL.
pub fn rpc_register(caller_tid: u32, server_channel: u32) -> Option<u32> {
    unsafe {
        for i in 0..MAX_PENDING_RPCS {
            if !RPC_PENDING[i].active {
                RPC_PENDING[i] = RpcPending {
                    caller_tid,
                    server_channel,
                    reply_buf: [0u8; RPC_MSG_MAX_LEN],
                    reply_len: 0,
                    active: true,
                    done: AtomicBool::new(false),
                };
                return Some(i as u32);
            }
        }
    }
    None
}

/// Complete an RPC call with a reply.
/// Called by kernel when processing SYS_IPC_REPLY.
/// Returns the caller_tid that should be woken up, or None if no matching RPC.
pub fn rpc_reply(caller_tid: u32, reply_data: &[u8]) -> Option<u32> {
    unsafe {
        for i in 0..MAX_PENDING_RPCS {
            let rpc = &mut RPC_PENDING[i];
            if rpc.active && rpc.caller_tid == caller_tid && !rpc.done.load(Ordering::Acquire) {
                let copy_len = reply_data.len().min(RPC_MSG_MAX_LEN);
                rpc.reply_buf[..copy_len].copy_from_slice(&reply_data[..copy_len]);
                rpc.reply_len = copy_len as u32;
                rpc.done.store(true, Ordering::Release);
                return Some(caller_tid);
            }
        }
    }
    None
}

/// Retrieve reply data for a completed RPC and free the slot.
/// Called by the woken-up caller to get the result.
/// Returns (reply_len, reply_buf) or None if not found/not done.
pub fn rpc_get_reply(caller_tid: u32, dst: &mut [u8]) -> Option<u32> {
    unsafe {
        for i in 0..MAX_PENDING_RPCS {
            let rpc = &mut RPC_PENDING[i];
            if rpc.active && rpc.caller_tid == caller_tid && rpc.done.load(Ordering::Acquire) {
                let copy_len = (rpc.reply_len as usize).min(dst.len());
                dst[..copy_len].copy_from_slice(&rpc.reply_buf[..copy_len]);
                let reply_len = rpc.reply_len;
                // Free the slot
                *rpc = RpcPending::empty();
                return Some(reply_len);
            }
        }
    }
    None
}

/// Cancel all pending RPCs for a task (called on task exit).
pub fn rpc_cancel_all(tid: u32) {
    unsafe {
        for i in 0..MAX_PENDING_RPCS {
            if RPC_PENDING[i].active && RPC_PENDING[i].caller_tid == tid {
                RPC_PENDING[i] = RpcPending::empty();
            }
        }
    }
}
