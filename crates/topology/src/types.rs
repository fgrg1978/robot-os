//! Types representing a parsed PHANES topology — RFC-0005.
//!
//! All structures are fixed-size and `Copy`; the topology is built
//! during boot in a stack/static buffer and never re-allocated.

use robot_os_abi::cap::{CapKind, CapPerms};
pub use robot_os_limits::{MAX_TASKS, MAX_CAPS_TOTAL};

use crate::AdmissionError;

// ──────────────────────────────────────────────────────────────────────────
// Bounds — fixed at compile time. RFC-0005.
// ──────────────────────────────────────────────────────────────────────────

/// Maximum scheduler classes per topology.
pub const MAX_CLASSES: usize = 8;

/// Maximum length of a task or class name (in bytes). Names beyond this
/// are rejected with `ParseError::NameTooLong`.
pub const MAX_TASK_NAME_LEN: usize = 32;

/// Maximum length of a cap-target string (e.g. `/cmd/motor`,
/// `bus.0/0x68`). Longer values are rejected.
pub const MAX_TARGET_LEN: usize = 64;

// ──────────────────────────────────────────────────────────────────────────
// Bounded string — borrows from input bytes
// ──────────────────────────────────────────────────────────────────────────

/// A small string borrowed from the input TOML buffer.
///
/// `MaybeStr` is `Copy` so it can live in the topology's static arrays
/// without owning anything. Equality compares the byte content.
#[derive(Clone, Copy, Debug)]
pub struct MaybeStr<'a> {
    bytes: &'a [u8],
}

impl<'a> MaybeStr<'a> {
    /// Construct from a byte slice. The bytes must be valid UTF-8 — no
    /// runtime check; the parser guarantees this.
    #[inline]
    pub const fn from_bytes(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    /// Borrow as `&str`. Safe because the parser only emits ASCII or
    /// validated UTF-8 ranges.
    #[inline]
    pub fn as_str(&self) -> &'a str {
        // The parser only accepts a printable ASCII subset for names and
        // targets, so this is always valid UTF-8.
        core::str::from_utf8(self.bytes).unwrap_or("")
    }

    /// Returns the underlying byte slice.
    #[inline]
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Returns `true` iff this string is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Length in bytes.
    #[inline]
    pub const fn len(&self) -> usize {
        self.bytes.len()
    }
}

impl PartialEq for MaybeStr<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl Eq for MaybeStr<'_> {}

impl<'a> PartialEq<&str> for MaybeStr<'a> {
    fn eq(&self, other: &&str) -> bool {
        self.bytes == other.as_bytes()
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Scheduler class
// ──────────────────────────────────────────────────────────────────────────

/// Scheduler-policy kind, RFC-0004.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum PolicyKind {
    /// Fixed-priority FIFO.
    Fifo = 0,
    /// Earliest-Deadline-First with Constant Bandwidth Server.
    Edf = 1,
    /// Round-robin with quantum.
    Rr = 2,
    /// Completely-Fair Scheduler-style fair share.
    Cfs = 3,
    /// Sporadic server.
    Sporadic = 4,
}

impl PolicyKind {
    /// Parse from the literal TOML string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "fifo" => Some(Self::Fifo),
            "edf" => Some(Self::Edf),
            "rr" => Some(Self::Rr),
            "cfs" => Some(Self::Cfs),
            "sporadic" => Some(Self::Sporadic),
            _ => None,
        }
    }
}

/// Preemption policy for a scheduler class.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Preemption {
    /// Always preempt on a higher-priority arrival.
    Always = 0,
    /// Preempt only when a timer fires.
    TimerOnly = 1,
    /// Never preempt (cooperative).
    Never = 2,
}

impl Preemption {
    /// Parse from the literal TOML string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "always" => Some(Self::Always),
            "timer-only" => Some(Self::TimerOnly),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

/// One scheduler class as declared in `SCHED.TOML`.
#[derive(Clone, Copy, Debug)]
pub struct ClassSpec<'a> {
    /// Class name, e.g. `"safety_critical"`. Borrowed from input.
    pub name: MaybeStr<'a>,
    /// Lower budget bound, percent of CPU per partition window.
    pub cpu_budget_min_pct: u8,
    /// Upper budget bound, percent of CPU per partition window.
    pub cpu_budget_max_pct: u8,
    /// Scheduling policy.
    pub policy: PolicyKind,
    /// Inclusive priority range `[lo, hi]` within the class.
    pub priority_range: (u8, u8),
    /// Preemption rule.
    pub preemption: Preemption,
    /// Round-robin time slice in milliseconds (only if `policy == Rr`).
    pub time_slice_ms: u16,
    /// Whether to reject task admissions that violate Liu-Layland.
    pub admission_control: bool,
}

impl<'a> ClassSpec<'a> {
    /// Construct an empty placeholder class. Used only to fill the
    /// fixed array; never seen by user code outside the parser.
    pub const fn empty() -> Self {
        Self {
            name: MaybeStr::from_bytes(&[]),
            cpu_budget_min_pct: 0,
            cpu_budget_max_pct: 0,
            policy: PolicyKind::Fifo,
            priority_range: (0, 0),
            preemption: Preemption::Always,
            time_slice_ms: 0,
            admission_control: false,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Task + cap specs
// ──────────────────────────────────────────────────────────────────────────

/// One capability grant declared in `CAPS.TOML`.
#[derive(Clone, Copy, Debug)]
pub struct CapSpec<'a> {
    /// Kind tag of the capability.
    pub kind: CapKind,
    /// Permission bits.
    pub perms: CapPerms,
    /// Target string, e.g. `/cmd/motor`, `motor.0`, `bus.0/0x68`.
    /// Resource-specific resolution happens during task spawn.
    pub target: MaybeStr<'a>,
}

impl<'a> CapSpec<'a> {
    /// Construct an empty placeholder.
    pub const fn empty() -> Self {
        Self {
            kind: CapKind::Null,
            perms: CapPerms::NONE,
            target: MaybeStr::from_bytes(&[]),
        }
    }
}

/// One task as declared in `CAPS.TOML`.
#[derive(Clone, Copy, Debug)]
pub struct TaskSpec<'a> {
    /// Task name, must match an entry in SCHED.TOML.
    pub name: MaybeStr<'a>,
    /// Scheduler-class name this task belongs to (cross-referenced after
    /// parsing both files).
    pub class_name: MaybeStr<'a>,
    /// Static priority within the class.
    pub priority: u8,
    /// Index into the topology's caps pool.
    pub caps_start: u16,
    /// Number of caps belonging to this task.
    pub caps_count: u16,
}

impl<'a> TaskSpec<'a> {
    /// Construct an empty placeholder.
    pub const fn empty() -> Self {
        Self {
            name: MaybeStr::from_bytes(&[]),
            class_name: MaybeStr::from_bytes(&[]),
            priority: 0,
            caps_start: 0,
            caps_count: 0,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Top-level topology
// ──────────────────────────────────────────────────────────────────────────

/// Optional global scheduler configuration declared at the top of
/// `SCHED.TOML`.
#[derive(Clone, Copy, Debug)]
pub struct SchedConfig {
    /// Adaptive Partitioning window in microseconds.
    pub partition_window_us: u32,
}

impl SchedConfig {
    /// RFC-0004 default: 10 ms window.
    pub const DEFAULT: Self = Self {
        partition_window_us: 10_000,
    };
}

/// Parsed topology — the kernel-internal in-memory form of CAPS.TOML +
/// SCHED.TOML.
///
/// Lifetime `'a` is the lifetime of the source TOML byte buffers; the
/// kernel stores those in BSS and never frees them post-boot.
pub struct Topology<'a> {
    classes: [ClassSpec<'a>; MAX_CLASSES],
    classes_len: u8,
    tasks: [TaskSpec<'a>; MAX_TASKS],
    tasks_len: u8,
    caps_pool: [CapSpec<'a>; MAX_CAPS_TOTAL],
    caps_pool_len: u16,
    sched_config: SchedConfig,
}

impl<'a> Topology<'a> {
    /// Construct an empty topology, ready to receive parsed entries.
    pub const fn empty() -> Self {
        Self {
            classes: [ClassSpec::empty(); MAX_CLASSES],
            classes_len: 0,
            tasks: [TaskSpec::empty(); MAX_TASKS],
            tasks_len: 0,
            caps_pool: [CapSpec::empty(); MAX_CAPS_TOTAL],
            caps_pool_len: 0,
            sched_config: SchedConfig::DEFAULT,
        }
    }

    /// Number of classes parsed.
    #[inline]
    pub fn classes_len(&self) -> usize {
        self.classes_len as usize
    }

    /// Number of tasks parsed.
    #[inline]
    pub fn tasks_len(&self) -> usize {
        self.tasks_len as usize
    }

    /// Total caps in the pool.
    #[inline]
    pub fn caps_pool_len(&self) -> usize {
        self.caps_pool_len as usize
    }

    /// Borrow the parsed classes (slice of valid entries only).
    pub fn classes(&self) -> &[ClassSpec<'a>] {
        &self.classes[..self.classes_len as usize]
    }

    /// Borrow the parsed tasks.
    pub fn tasks(&self) -> &[TaskSpec<'a>] {
        &self.tasks[..self.tasks_len as usize]
    }

    /// Borrow the caps belonging to a given task.
    pub fn caps_of(&self, task: &TaskSpec<'a>) -> &[CapSpec<'a>] {
        let start = task.caps_start as usize;
        let end = start + task.caps_count as usize;
        &self.caps_pool[start..end]
    }

    /// Get the global scheduler config (window, etc.).
    #[inline]
    pub fn sched_config(&self) -> SchedConfig {
        self.sched_config
    }

    /// Look up a class by name. O(`classes_len`) but `classes_len ≤ 8`.
    pub fn find_class(&self, name: &MaybeStr<'a>) -> Option<&ClassSpec<'a>> {
        self.classes().iter().find(|c| c.name == *name)
    }

    /// Look up a task by name.
    pub fn find_task(&self, name: &MaybeStr<'a>) -> Option<&TaskSpec<'a>> {
        self.tasks().iter().find(|t| t.name == *name)
    }

    /// Append a class. Returns `Err` if the table is full or the name
    /// duplicates an existing class.
    pub fn push_class(&mut self, class: ClassSpec<'a>) -> Result<(), AdmissionError> {
        if (self.classes_len as usize) >= MAX_CLASSES {
            return Err(AdmissionError::DuplicateClass); // closest existing variant
        }
        if self.find_class(&class.name).is_some() {
            return Err(AdmissionError::DuplicateClass);
        }
        if class.priority_range.0 > class.priority_range.1 {
            return Err(AdmissionError::InvalidPriorityRange);
        }
        self.classes[self.classes_len as usize] = class;
        self.classes_len += 1;
        Ok(())
    }

    /// Append a task and consume its caps from the parser. The caller
    /// passes the caps inline.
    pub fn push_task(
        &mut self,
        name: MaybeStr<'a>,
        class_name: MaybeStr<'a>,
        priority: u8,
        caps: &[CapSpec<'a>],
    ) -> Result<(), AdmissionError> {
        if (self.tasks_len as usize) >= MAX_TASKS {
            return Err(AdmissionError::DuplicateTask);
        }
        if self.find_task(&name).is_some() {
            return Err(AdmissionError::DuplicateTask);
        }
        let new_pool_len = (self.caps_pool_len as usize) + caps.len();
        if new_pool_len > MAX_CAPS_TOTAL {
            return Err(AdmissionError::DuplicateTask);
        }
        let caps_start = self.caps_pool_len;
        for (i, c) in caps.iter().enumerate() {
            if matches!(c.kind, CapKind::Null) {
                return Err(AdmissionError::UnknownCapKind);
            }
            self.caps_pool[caps_start as usize + i] = *c;
        }
        self.caps_pool_len = new_pool_len as u16;
        self.tasks[self.tasks_len as usize] = TaskSpec {
            name,
            class_name,
            priority,
            caps_start,
            caps_count: caps.len() as u16,
        };
        self.tasks_len += 1;
        Ok(())
    }

    /// Set the global scheduler config (called by parser when SCHED.TOML
    /// declares one).
    pub fn set_sched_config(&mut self, cfg: SchedConfig) {
        self.sched_config = cfg;
    }

    /// Cross-cutting admission check. Returns the **first** error
    /// found; the caller is expected to abort boot on `Err`.
    ///
    /// Checks performed:
    ///
    /// - Total class budgets do not exceed 100 %.
    /// - Every task references a known class.
    pub fn admission_check(&self) -> Result<(), AdmissionError> {
        let mut total: u32 = 0;
        for class in self.classes() {
            total += class.cpu_budget_min_pct as u32;
        }
        if total > 100 {
            return Err(AdmissionError::BudgetOverflow);
        }
        for task in self.tasks() {
            if self.find_class(&task.class_name).is_none() {
                return Err(AdmissionError::UnknownClass);
            }
        }
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_topology_is_consistent() {
        let t = Topology::empty();
        assert_eq!(t.classes_len(), 0);
        assert_eq!(t.tasks_len(), 0);
        assert_eq!(t.caps_pool_len(), 0);
        assert!(t.admission_check().is_ok());
    }

    #[test]
    fn duplicate_class_rejected() {
        let mut t = Topology::empty();
        let c = ClassSpec {
            name: MaybeStr::from_bytes(b"safety"),
            cpu_budget_min_pct: 20,
            cpu_budget_max_pct: 100,
            policy: PolicyKind::Fifo,
            priority_range: (0, 7),
            preemption: Preemption::Always,
            time_slice_ms: 0,
            admission_control: false,
        };
        t.push_class(c).unwrap();
        assert_eq!(t.push_class(c), Err(AdmissionError::DuplicateClass));
    }

    #[test]
    fn budget_overflow_caught() {
        // MaybeStr borrows; sources must outlive the Topology.
        const CLASS_NAMES: [&[u8]; 3] = [b"c0", b"c1", b"c2"];
        let mut t = Topology::empty();
        for name in CLASS_NAMES {
            t.push_class(ClassSpec {
                name: MaybeStr::from_bytes(name),
                cpu_budget_min_pct: 50,
                cpu_budget_max_pct: 100,
                policy: PolicyKind::Fifo,
                priority_range: (0, 7),
                preemption: Preemption::Always,
                time_slice_ms: 0,
                admission_control: false,
            })
            .unwrap();
        }
        // 50 + 50 + 50 = 150 > 100
        assert_eq!(
            t.admission_check(),
            Err(AdmissionError::BudgetOverflow)
        );
    }

    #[test]
    fn task_must_reference_known_class() {
        let mut t = Topology::empty();
        t.push_task(
            MaybeStr::from_bytes(b"motor_loop"),
            MaybeStr::from_bytes(b"hard_rt"),
            5,
            &[CapSpec {
                kind: CapKind::Pwm,
                perms: CapPerms::RW,
                target: MaybeStr::from_bytes(b"motor.0"),
            }],
        )
        .unwrap();
        assert_eq!(
            t.admission_check(),
            Err(AdmissionError::UnknownClass)
        );
    }

    #[test]
    fn caps_of_returns_correct_slice() {
        let mut t = Topology::empty();
        t.push_task(
            MaybeStr::from_bytes(b"a"),
            MaybeStr::from_bytes(b"any"),
            0,
            &[
                CapSpec {
                    kind: CapKind::Channel,
                    perms: CapPerms::READ,
                    target: MaybeStr::from_bytes(b"/x"),
                },
                CapSpec {
                    kind: CapKind::Channel,
                    perms: CapPerms::WRITE,
                    target: MaybeStr::from_bytes(b"/y"),
                },
            ],
        )
        .unwrap();
        let task = &t.tasks()[0];
        let caps = t.caps_of(task);
        assert_eq!(caps.len(), 2);
        assert_eq!(caps[0].target, MaybeStr::from_bytes(b"/x"));
        assert_eq!(caps[1].target, MaybeStr::from_bytes(b"/y"));
    }

    #[test]
    fn null_cap_kind_rejected() {
        let mut t = Topology::empty();
        let r = t.push_task(
            MaybeStr::from_bytes(b"a"),
            MaybeStr::from_bytes(b"any"),
            0,
            &[CapSpec {
                kind: CapKind::Null,
                perms: CapPerms::READ,
                target: MaybeStr::from_bytes(b"/x"),
            }],
        );
        assert_eq!(r, Err(AdmissionError::UnknownCapKind));
    }

    #[test]
    fn policy_parse_round_trip() {
        assert_eq!(PolicyKind::from_str("edf"), Some(PolicyKind::Edf));
        assert_eq!(PolicyKind::from_str("cfs"), Some(PolicyKind::Cfs));
        assert_eq!(PolicyKind::from_str("nope"), None);
    }
}
