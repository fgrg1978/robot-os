//! PHANES static topology — RFC-0005.
//!
//! At boot, the kernel loads two signed TOML files (`CAPS.TOML` and
//! `SCHED.TOML`) and parses them into a fixed-pool, alloc-free
//! `Topology` structure. Every user-space task spawned thereafter
//! takes its capability table and scheduler class from this structure;
//! no runtime discovery is permitted in safety mode.
//!
//! # Crate layout
//!
//! - [`types`] — `Topology`, `ClassSpec`, `TaskSpec`, `CapSpec`, fixed
//!   pool sizes, lookup helpers.
//! - [`parser`] — alloc-free TOML subset parser.
//! - [`verify`] — Ed25519 signature verification of the TOML bytes
//!   against the trusted topology key.
//!
//! # Lifecycle
//!
//! ```text
//!   ┌─────────────────────────────────────────────────────────────┐
//!   │ Boot                                                        │
//!   │                                                             │
//!   │  1. Load CAPS.TOML + CAPS.TOML.SIG from FAT32               │
//!   │  2. Load SCHED.TOML + SCHED.TOML.SIG from FAT32             │
//!   │  3. verify::verify_signature(&toml, &sig, &TRUSTED_KEY)?    │
//!   │  4. parser::parse_caps(&caps_toml, &mut topology)?          │
//!   │  5. parser::parse_sched(&sched_toml, &mut topology)?        │
//!   │  6. topology.admission_check()?                              │
//!   │  7. STATIC_TOPOLOGY = topology  (immutable thereafter)      │
//!   │                                                             │
//!   │  Any failure ⇒ kernel halts; user-space spawn is blocked.   │
//!   └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Memory budget
//!
//! Worst case: 8 classes + 64 tasks + 1024 caps total + 64 KiB
//! source-text buffer ≈ ~96 KiB static, all stored in BSS.

#![no_std]
#![deny(missing_docs)]

pub mod builder;
pub mod parser;
pub mod state;
pub mod types;
pub mod verify;

pub use builder::default_minimal;
pub use parser::{parse_caps, parse_sched, ParseError};
pub use state::{get, init, is_ready, InitError};
pub use types::{
    CapSpec, ClassSpec, MaybeStr, PolicyKind, Preemption, SchedConfig,
    TaskSpec, Topology, MAX_CAPS_TOTAL, MAX_CLASSES, MAX_TASKS,
    MAX_TASK_NAME_LEN,
};
pub use verify::{verify_signature, VerifyError};

/// Top-level errors a boot loader is expected to discriminate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TopologyError {
    /// CAPS.TOML or SCHED.TOML signature verification failed.
    Signature(VerifyError),
    /// Parser error (syntax, oversized, unknown field).
    Parse(ParseError),
    /// Admission-control failure (e.g., class budgets sum > 100 %, task
    /// references a non-existent class, duplicate task name).
    Admission(AdmissionError),
}

/// Admission-control errors detected after parsing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AdmissionError {
    /// Class budgets sum to more than 100 %.
    BudgetOverflow,
    /// A task references a scheduler class not declared.
    UnknownClass,
    /// Two tasks share the same name.
    DuplicateTask,
    /// Two classes share the same name.
    DuplicateClass,
    /// A class's `priority_range` is empty or inverted.
    InvalidPriorityRange,
    /// CapSpec references an unknown kind tag.
    UnknownCapKind,
}

impl From<VerifyError> for TopologyError {
    fn from(e: VerifyError) -> Self {
        Self::Signature(e)
    }
}

impl From<ParseError> for TopologyError {
    fn from(e: ParseError) -> Self {
        Self::Parse(e)
    }
}

impl From<AdmissionError> for TopologyError {
    fn from(e: AdmissionError) -> Self {
        Self::Admission(e)
    }
}
