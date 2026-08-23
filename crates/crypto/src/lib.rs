//! Cryptographic primitives for Robot OS (F07).
//!
//! Pure `no_std` software implementations — no external dependencies.
//! Suitable for bare-metal RISC-V targets.

#![no_std]

pub mod ct;
pub mod sha256;
pub mod aes;
pub mod x25519;
pub mod secure_channel;
pub mod ed25519;
