#![no_std]

//! Service manager — port of kernel/core/service.c
//!
//! Microservice registry: register, discover, stop, heartbeat.
//! Services communicate via IPC channels.

use robot_os_sync::SpinLock;
pub use robot_os_limits::MAX_SERVICES;
pub const MAX_SERVICE_NAME: usize = 32;

#[derive(Clone, Copy, PartialEq)]
pub enum ServiceState {
    Free    = 0,
    Running = 1,
    Stopped = 2,
}

#[derive(Clone, Copy)]
pub struct ServiceEntry {
    pub name:        [u8; MAX_SERVICE_NAME],
    pub tid:         u32,
    pub state:       ServiceState,
    pub heartbeat:   u32,
    pub ipc_channel: u32,
}

impl ServiceEntry {
    pub const fn new() -> Self {
        ServiceEntry {
            name:        [0u8; MAX_SERVICE_NAME],
            tid:         0,
            state:       ServiceState::Free,
            heartbeat:   0,
            ipc_channel: 0,
        }
    }
}

struct ServiceTable {
    entries: [ServiceEntry; MAX_SERVICES],
    count:   usize,
}

impl ServiceTable {
    const fn new() -> Self {
        ServiceTable {
            entries: [ServiceEntry::new(); MAX_SERVICES],
            count:   0,
        }
    }

    fn find_by_name(&self, name: &[u8]) -> Option<usize> {
        let query_len = name.len().min(MAX_SERVICE_NAME);
        for i in 0..MAX_SERVICES {
            if self.entries[i].state == ServiceState::Free { continue; }
            let ent_name = &self.entries[i].name;
            // Compare byte-by-byte up to query length, then check null terminator
            if ent_name[..query_len] == name[..query_len] {
                if query_len == MAX_SERVICE_NAME || ent_name[query_len] == 0 {
                    return Some(i);
                }
            }
        }
        None
    }

    fn find_free(&self) -> Option<usize> {
        for i in 0..MAX_SERVICES {
            if self.entries[i].state == ServiceState::Free { return Some(i); }
        }
        None
    }
}

static SERVICE_TABLE: SpinLock<ServiceTable> = SpinLock::new(ServiceTable::new());

pub fn service_init() {
    // Static initialization handles zero-fill. Nothing extra needed.
}

/// Register a new service with the given name, task ID and IPC channel.
/// Returns 0 on success, -1 if name already registered or table full.
pub fn service_register(name: &[u8], tid: u32, channel: u32) -> i32 {
    let mut t = SERVICE_TABLE.lock();
    if t.find_by_name(name).is_some() { return -1; }
    let idx = match t.find_free() {
        Some(i) => i,
        None    => return -1,
    };
    let e = &mut t.entries[idx];
    let n = name.len().min(MAX_SERVICE_NAME);
    e.name[..n].copy_from_slice(&name[..n]);
    if n < MAX_SERVICE_NAME { e.name[n] = 0; }
    e.tid         = tid;
    e.state       = ServiceState::Running;
    e.heartbeat   = 0;
    e.ipc_channel = channel;
    t.count += 1;
    0
}

/// Look up a service by name.  Returns a copy of the entry or None.
pub fn service_discover(name: &[u8]) -> Option<ServiceEntry> {
    let t = SERVICE_TABLE.lock();
    t.find_by_name(name).map(|i| t.entries[i])
}

/// Stop (but don't remove) a service.
pub fn service_stop(name: &[u8]) -> i32 {
    let mut t = SERVICE_TABLE.lock();
    match t.find_by_name(name) {
        Some(i) => { t.entries[i].state = ServiceState::Stopped; 0 }
        None    => -1,
    }
}

/// Restart a previously stopped service.
pub fn service_restart(name: &[u8], tid: u32) -> i32 {
    let mut t = SERVICE_TABLE.lock();
    match t.find_by_name(name) {
        Some(i) => {
            t.entries[i].tid   = tid;
            t.entries[i].state = ServiceState::Running;
            0
        }
        None => -1,
    }
}

/// Record a heartbeat from a running service.
pub fn service_heartbeat(name: &[u8]) -> i32 {
    let mut t = SERVICE_TABLE.lock();
    match t.find_by_name(name) {
        Some(i) => {
            t.entries[i].heartbeat = t.entries[i].heartbeat.wrapping_add(1);
            0
        }
        None => -1,
    }
}

/// Return the number of registered services (Running or Stopped).
pub fn service_count() -> usize {
    SERVICE_TABLE.lock().count
}

/// List all services (calls `cb` for each entry).
pub fn service_list(mut cb: impl FnMut(&ServiceEntry)) {
    let t = SERVICE_TABLE.lock();
    for i in 0..MAX_SERVICES {
        if t.entries[i].state != ServiceState::Free {
            cb(&t.entries[i]);
        }
    }
}
