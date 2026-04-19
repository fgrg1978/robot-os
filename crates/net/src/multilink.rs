//! E02 — Multi-link Transport abstraction (WiFi/LoRa/RF failover).
//!
//! Defines the [`Transport`] trait implemented by each physical link
//! (WiFi/Ethernet TCP, LoRa over UART, RF modems, …) and a
//! [`MultiLinkTransport`] that owns up to [`MAX_LINKS`] of them and
//! transparently fails over when the currently active link degrades.
//!
//! Design:
//!   * Each transport reports `is_up()` and `link_quality()` (0..=255).
//!   * The multiplexer keeps them ordered by priority (index 0 = primary).
//!   * On every send/recv we track consecutive failures and last-RX time.
//!   * A link is marked "down" after
//!     [`TRANSPORT_MAX_CONSEC_FAILURES`] failed sends **or** no RX for
//!     [`TRANSPORT_FAILOVER_TIMEOUT_TICKS`] ticks.
//!   * When the active link goes down we fall back to the next-priority
//!     link that reports `is_up()`.  Every
//!     [`LINK_PROBE_INTERVAL_TICKS`] ticks we probe the (higher-priority)
//!     primary and switch back as soon as it recovers.
//!
//! The implementation is `#![no_std]` and heap-free: all state lives in
//! fixed-size arrays.  A "tick" here is a monotonic counter supplied by
//! the caller (typically milliseconds from the CLINT).

// ── Tunables (no magic numbers) ────────────────────────────────────────────

/// Maximum number of physical links a [`MultiLinkTransport`] can hold.
/// Wheeled robot typically uses 3 links (WiFi + LoRa + RF); we reserve
/// one extra slot for future drone radios.
pub const MAX_LINKS: usize = 4;

/// Consecutive failed `send()` calls after which a link is considered down.
pub const TRANSPORT_MAX_CONSEC_FAILURES: u8 = 3;

/// No-RX timeout, in caller-supplied ticks (ms), after which a link is
/// considered down even if sends are reported as "successful" (useful for
/// half-open TCP connections).
pub const TRANSPORT_FAILOVER_TIMEOUT_TICKS: u64 = 5_000;

/// How often to re-probe a down primary link to see if it's back.
pub const LINK_PROBE_INTERVAL_TICKS: u64 = 2_000;

/// Link quality value returned when the underlying driver has no
/// meaningful signal metric (e.g. stub / UART transport).
pub const LINK_QUALITY_UNKNOWN: u8 = 128;

/// Link quality reported by a transport that is definitely down.
pub const LINK_QUALITY_DOWN: u8 = 0;

/// Link quality reported by a transport that is up and healthy but has
/// no extra signal information (e.g. plain Ethernet/UART).
pub const LINK_QUALITY_GOOD: u8 = 200;

// ── Error type ─────────────────────────────────────────────────────────────

/// Transport-layer error codes (kept simple — no `std::io::Error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// Link reports `is_up() == false` (no hardware / not associated).
    NotReady,
    /// Underlying driver reported a transient failure.
    WouldBlock,
    /// Underlying driver reported a fatal send/recv error.
    Io,
    /// Caller buffer too small for the available data.
    BufTooSmall,
}

// ── Transport trait ────────────────────────────────────────────────────────

/// A single physical transport channel (WiFi / LoRa / RF).
///
/// Implementations MUST be non-blocking — `recv()` returns `WouldBlock`
/// when no data is available rather than busy-looping.
pub trait Transport {
    /// Send `data`.  Returns number of bytes written on success.
    fn send(&mut self, data: &[u8]) -> Result<usize, TransportError>;

    /// Receive into `buf`.  Returns number of bytes read on success.
    /// A `WouldBlock` return means "no data yet, try later".
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, TransportError>;

    /// `true` while the underlying hardware / link is associated.
    fn is_up(&self) -> bool;

    /// Signal strength / quality indicator, 0 = worst, 255 = best.
    /// Drivers without a real metric should return
    /// [`LINK_QUALITY_GOOD`] when up, [`LINK_QUALITY_DOWN`] otherwise.
    fn link_quality(&self) -> u8;

    /// Short human-readable identifier — useful for diagnostics.
    /// Defaults to the empty string.
    fn name(&self) -> &'static str { "" }
}

// ── Per-link bookkeeping ───────────────────────────────────────────────────

/// Internal tracking wrapper stored inside [`MultiLinkTransport`].
struct LinkSlot<'a> {
    transport:       &'a mut dyn Transport,
    priority:        u8,
    consec_failures: u8,
    last_rx_tick:    u64,
    last_probe_tick: u64,
    down_marked:     bool,
}

impl<'a> LinkSlot<'a> {
    fn new(transport: &'a mut dyn Transport, priority: u8, now_ticks: u64) -> Self {
        Self {
            transport,
            priority,
            consec_failures: 0,
            last_rx_tick:    now_ticks,
            last_probe_tick: now_ticks,
            down_marked:     false,
        }
    }

    /// Update the "down" flag based on health counters.  Returns the new
    /// value of `down_marked`.
    fn refresh_health(&mut self, now_ticks: u64) -> bool {
        let hw_down = !self.transport.is_up();
        // Only consider RX staleness once the link has received at least
        // one byte; otherwise a fresh link would be marked down instantly.
        let rx_started = self.last_rx_tick > 0;
        let rx_stale = rx_started
            && now_ticks.saturating_sub(self.last_rx_tick)
                >= TRANSPORT_FAILOVER_TIMEOUT_TICKS;
        let too_many_fails = self.consec_failures >= TRANSPORT_MAX_CONSEC_FAILURES;
        self.down_marked = hw_down || rx_stale || too_many_fails;
        self.down_marked
    }
}

// ── MultiLinkTransport ─────────────────────────────────────────────────────

/// Multiplexes N [`Transport`]s with automatic priority-based failover.
///
/// `tick()` must be fed a monotonic counter (milliseconds recommended).
/// All public methods are non-blocking.
pub struct MultiLinkTransport<'a> {
    links:      [Option<LinkSlot<'a>>; MAX_LINKS],
    link_count: usize,
    active_idx: usize,
    now_ticks:  u64,
}

impl<'a> Default for MultiLinkTransport<'a> {
    fn default() -> Self { Self::new() }
}

impl<'a> MultiLinkTransport<'a> {
    /// Create an empty multiplexer.
    pub const fn new() -> Self {
        Self {
            links: [
                const { None },
                const { None },
                const { None },
                const { None },
            ],
            link_count: 0,
            active_idx: 0,
            now_ticks:  0,
        }
    }

    /// Register a new transport.  Lower `priority` = preferred.
    /// Returns `Err(())` if the mux is full.
    pub fn add_link(
        &mut self,
        transport: &'a mut dyn Transport,
        priority: u8,
    ) -> Result<(), ()> {
        if self.link_count >= MAX_LINKS {
            return Err(());
        }
        self.links[self.link_count] =
            Some(LinkSlot::new(transport, priority, self.now_ticks));
        self.link_count += 1;
        self.sort_by_priority();
        Ok(())
    }

    /// Update the internal clock.  Call this before every
    /// [`send`](Self::send) / [`recv`](Self::recv) / [`poll`](Self::poll).
    pub fn tick(&mut self, now_ticks: u64) {
        self.now_ticks = now_ticks;
    }

    /// Number of registered links.
    pub fn link_count(&self) -> usize { self.link_count }

    /// Index of the currently active link (primary first on boot).
    pub fn active_index(&self) -> usize { self.active_idx }

    /// Returns name of the active link, or `""` if none.
    pub fn active_name(&self) -> &'static str {
        self.links[self.active_idx]
            .as_ref()
            .map(|s| s.transport.name())
            .unwrap_or("")
    }

    /// Quality of the currently active link.
    pub fn active_quality(&self) -> u8 {
        self.links[self.active_idx]
            .as_ref()
            .map(|s| s.transport.link_quality())
            .unwrap_or(LINK_QUALITY_DOWN)
    }

    /// Send `data` over the active link; if that fails, fall back to the
    /// next-healthiest link and retry.
    pub fn send(&mut self, data: &[u8]) -> Result<usize, TransportError> {
        self.poll();
        // Try the active link first.
        if self.try_send(self.active_idx, data).is_ok() {
            return Ok(data.len());
        }
        // Active link failed — try every other link in priority order.
        for idx in 0..self.link_count {
            if idx == self.active_idx { continue; }
            if self.link_is_up(idx) && self.try_send(idx, data).is_ok() {
                self.switch_to(idx);
                return Ok(data.len());
            }
        }
        Err(TransportError::NotReady)
    }

    /// Receive from the active link.
    pub fn recv(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        self.poll();
        let idx = self.active_idx;
        let now = self.now_ticks;
        let slot = match self.links[idx].as_mut() {
            Some(s) => s,
            None    => return Err(TransportError::NotReady),
        };
        match slot.transport.recv(buf) {
            Ok(n) if n > 0 => {
                slot.last_rx_tick    = now;
                slot.consec_failures = 0;
                slot.down_marked     = false;
                Ok(n)
            }
            Ok(_) => Err(TransportError::WouldBlock),
            Err(TransportError::WouldBlock) => Err(TransportError::WouldBlock),
            Err(e) => {
                slot.consec_failures = slot.consec_failures.saturating_add(1);
                Err(e)
            }
        }
    }

    /// Re-evaluate health of all links and, if the active one is down
    /// while a higher-priority link has come back, fail back.
    pub fn poll(&mut self) {
        let now = self.now_ticks;
        for slot_opt in self.links.iter_mut().take(self.link_count) {
            if let Some(slot) = slot_opt.as_mut() {
                slot.refresh_health(now);
            }
        }

        // Fail over: if the active link is dead, pick the best healthy one.
        let active_down = self.links[self.active_idx]
            .as_ref()
            .map(|s| s.down_marked)
            .unwrap_or(true);
        if active_down {
            if let Some(next) = self.find_healthy(self.active_idx) {
                self.switch_to(next);
            }
        }

        // Fail back: periodically try to return to a higher-priority link.
        for idx in 0..self.active_idx {
            let should_probe = self.links[idx].as_ref()
                .map(|s| now.saturating_sub(s.last_probe_tick)
                    >= LINK_PROBE_INTERVAL_TICKS)
                .unwrap_or(false);
            if should_probe {
                if let Some(slot) = self.links[idx].as_mut() {
                    slot.last_probe_tick = now;
                    if slot.transport.is_up() {
                        slot.consec_failures = 0;
                        slot.last_rx_tick    = now;
                        slot.down_marked     = false;
                        self.switch_to(idx);
                        break;
                    }
                }
            }
        }
    }

    // ── internal helpers ───────────────────────────────────────────────────

    fn try_send(&mut self, idx: usize, data: &[u8])
        -> Result<(), TransportError>
    {
        let slot = match self.links[idx].as_mut() {
            Some(s) => s,
            None    => return Err(TransportError::NotReady),
        };
        if !slot.transport.is_up() {
            slot.consec_failures = slot.consec_failures.saturating_add(1);
            return Err(TransportError::NotReady);
        }
        match slot.transport.send(data) {
            Ok(_) => {
                slot.consec_failures = 0;
                Ok(())
            }
            Err(e) => {
                slot.consec_failures = slot.consec_failures.saturating_add(1);
                Err(e)
            }
        }
    }

    fn link_is_up(&self, idx: usize) -> bool {
        self.links[idx]
            .as_ref()
            .map(|s| !s.down_marked && s.transport.is_up())
            .unwrap_or(false)
    }

    fn find_healthy(&self, skip: usize) -> Option<usize> {
        for idx in 0..self.link_count {
            if idx == skip { continue; }
            if self.link_is_up(idx) { return Some(idx); }
        }
        None
    }

    fn switch_to(&mut self, idx: usize) {
        if idx == self.active_idx { return; }
        self.active_idx = idx;
        if let Some(slot) = self.links[idx].as_mut() {
            slot.consec_failures = 0;
            slot.last_rx_tick    = self.now_ticks;
            slot.down_marked     = false;
        }
    }

    fn sort_by_priority(&mut self) {
        // Insertion sort — `link_count` is at most [`MAX_LINKS`].
        for i in 1..self.link_count {
            let mut j = i;
            while j > 0 {
                let swap = match (&self.links[j-1], &self.links[j]) {
                    (Some(a), Some(b)) => a.priority > b.priority,
                    _ => false,
                };
                if swap {
                    self.links.swap(j-1, j);
                    j -= 1;
                } else {
                    break;
                }
            }
        }
    }
}
