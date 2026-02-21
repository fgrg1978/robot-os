/// Pipe — port of kernel/core/pipe.c + kernel/include/pipe.h

use robot_os_sync::SpinLock;

pub const PIPE_BUF_SIZE: usize = 4096;
pub const MAX_PIPES:     usize = 32;

#[derive(Copy, Clone, PartialEq)]
pub enum PipeState {
    Free         = 0,
    Active       = 1,
    ReadClosed   = 2,
    WriteClosed  = 3,
    Closed       = 4,
}

#[derive(Copy, Clone)]
pub struct Pipe {
    pub buffer:          [u8; PIPE_BUF_SIZE],
    pub read_pos:        u32,
    pub write_pos:       u32,
    pub state:           PipeState,
    pub readers:         u32,
    pub writers:         u32,
    pub waiting_readers: u32,
    pub waiting_writers: u32,
    pub id:              u32,
}

impl Pipe {
    pub const fn zeroed() -> Self {
        Pipe {
            buffer:          [0u8; PIPE_BUF_SIZE],
            read_pos:        0,
            write_pos:       0,
            state:           PipeState::Free,
            readers:         0,
            writers:         0,
            waiting_readers: 0,
            waiting_writers: 0,
            id:              0,
        }
    }

    pub fn is_empty(&self) -> bool { self.read_pos == self.write_pos }

    pub fn is_full(&self) -> bool {
        ((self.write_pos + 1) as usize % PIPE_BUF_SIZE) == self.read_pos as usize
    }

    pub fn available(&self) -> usize {
        let w = self.write_pos as usize;
        let r = self.read_pos as usize;
        if w >= r { w - r } else { PIPE_BUF_SIZE - r + w }
    }

    pub fn space(&self) -> usize {
        PIPE_BUF_SIZE - 1 - self.available()
    }
}

// ── Global pipe pool ──────────────────────────────────────────────────────────

struct PipePool {
    pipes:   [Pipe; MAX_PIPES],
    next_id: u32,
}

impl PipePool {
    const fn new() -> Self {
        PipePool {
            pipes:   [Pipe::zeroed(); MAX_PIPES],
            next_id: 1,
        }
    }

    fn alloc(&mut self) -> Option<usize> {
        for i in 0..MAX_PIPES {
            if self.pipes[i].state == PipeState::Free {
                self.pipes[i] = Pipe::zeroed();
                self.pipes[i].state = PipeState::Active;
                self.pipes[i].id    = self.next_id;
                self.next_id       += 1;
                return Some(i);
            }
        }
        None
    }
}

static PIPES: SpinLock<PipePool> = SpinLock::new(PipePool::new());

// ── Public API ────────────────────────────────────────────────────────────────

pub fn pipe_init() {
    let mut pool = PIPES.lock();
    for i in 0..MAX_PIPES {
        pool.pipes[i].state = PipeState::Free;
        pool.pipes[i].id    = 0;
    }
}

/// Create a pipe; returns (read_idx, write_idx) on success.
pub fn pipe_create() -> Option<(usize, usize)> {
    let mut pool = PIPES.lock();
    let idx = pool.alloc()?;
    pool.pipes[idx].readers = 1;
    pool.pipes[idx].writers = 1;
    Some((idx, idx))  // Same slot — read/write ends distinguished by caller
}

/// Read up to `count` bytes.  Returns bytes read, 0 on EOF, -1 on error.
pub fn pipe_read(idx: usize, buf: *mut u8, count: usize) -> i32 {
    if idx >= MAX_PIPES { return -1; }
    let mut pool = PIPES.lock();
    let pipe = &mut pool.pipes[idx];

    if pipe.state == PipeState::Free { return -1; }

    // No data available
    if pipe.is_empty() {
        return if pipe.state == PipeState::WriteClosed || pipe.state == PipeState::Closed {
            0   // EOF: write end closed, no more data coming
        } else {
            -2  // EAGAIN: would block, writer still alive
        };
    }

    let avail = pipe.available();
    let to_read = count.min(avail);
    for i in 0..to_read {
        unsafe {
            *buf.add(i) = pipe.buffer[pipe.read_pos as usize % PIPE_BUF_SIZE];
        }
        pipe.read_pos = (pipe.read_pos + 1) % PIPE_BUF_SIZE as u32;
    }
    to_read as i32
}

/// Write up to `count` bytes.  Returns bytes written, -1 on error.
pub fn pipe_write(idx: usize, buf: *const u8, count: usize) -> i32 {
    if idx >= MAX_PIPES { return -1; }
    let mut pool = PIPES.lock();
    let pipe = &mut pool.pipes[idx];

    if pipe.state == PipeState::Free
        || pipe.state == PipeState::ReadClosed
        || pipe.state == PipeState::Closed
    {
        return -1;  // EPIPE
    }

    let space = pipe.space();
    let to_write = count.min(space);
    for i in 0..to_write {
        pipe.buffer[pipe.write_pos as usize % PIPE_BUF_SIZE] = unsafe { *buf.add(i) };
        pipe.write_pos = (pipe.write_pos + 1) % PIPE_BUF_SIZE as u32;
    }
    to_write as i32
}

pub fn pipe_close_read(idx: usize) {
    if idx >= MAX_PIPES { return; }
    let mut pool = PIPES.lock();
    let pipe = &mut pool.pipes[idx];
    if pipe.readers > 0 { pipe.readers -= 1; }
    if pipe.readers == 0 {
        pipe.state = if pipe.writers == 0 { PipeState::Closed } else { PipeState::ReadClosed };
    }
}

pub fn pipe_close_write(idx: usize) {
    if idx >= MAX_PIPES { return; }
    let mut pool = PIPES.lock();
    let pipe = &mut pool.pipes[idx];
    if pipe.writers > 0 { pipe.writers -= 1; }
    if pipe.writers == 0 {
        pipe.state = if pipe.readers == 0 { PipeState::Closed } else { PipeState::WriteClosed };
    }
}

pub fn pipe_available(idx: usize) -> usize {
    if idx >= MAX_PIPES { return 0; }
    PIPES.lock().pipes[idx].available()
}

pub fn pipe_space(idx: usize) -> usize {
    if idx >= MAX_PIPES { return 0; }
    PIPES.lock().pipes[idx].space()
}
