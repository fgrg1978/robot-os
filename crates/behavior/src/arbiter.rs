//! Subsumption arbiter — iterates layers L0→L3, first `valid` output wins.
//!
//! Layer 0 (emergency stop) is always enabled and cannot be disabled.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use crate::types::*;
use crate::layers;

/// Per-layer enable flags.  Index 0 is always true.
static LAYER_ENABLED: [AtomicBool; NUM_LAYERS] = [
    AtomicBool::new(true),  // L0: emergency-stop — always on
    AtomicBool::new(true),  // L1: avoid-obstacle
    AtomicBool::new(true),  // L2: remote-vla
    AtomicBool::new(true),  // L3: explore
];

/// Last winning layer (for status display).
static LAST_WINNER: AtomicU8 = AtomicU8::new(0xFF);

/// Layer names for display.
pub const LAYER_NAMES: [&str; NUM_LAYERS] = [
    "emergency-stop",
    "avoid-obstacle",
    "remote-vla",
    "explore",
];

/// Run the subsumption arbiter.  Iterates L0→L3; first layer that produces
/// a valid output wins.
pub fn arbitrate(state: &SensorState, _mlp: &MlpResult) -> BehaviorOutput {
    // L0: emergency stop — always runs, cannot be disabled
    {
        let out = layers::layer_emergency_stop(state);
        if out.cmd.valid {
            LAST_WINNER.store(0, Ordering::Relaxed);
            return out;
        }
    }

    // L1: avoid obstacle (MLP-based, only if no-ml is not set)
    #[cfg(not(feature = "no-ml"))]
    if LAYER_ENABLED[1].load(Ordering::Relaxed) {
        let out = layers::layer_avoid_obstacle(state, _mlp);
        if out.cmd.valid {
            LAST_WINNER.store(1, Ordering::Relaxed);
            return out;
        }
    }

    // L2: remote VLA
    if LAYER_ENABLED[2].load(Ordering::Relaxed) {
        let out = layers::layer_remote_vla(state);
        if out.cmd.valid {
            LAST_WINNER.store(2, Ordering::Relaxed);
            return out;
        }
    }

    // L3: explore (default)
    if LAYER_ENABLED[3].load(Ordering::Relaxed) {
        let out = layers::layer_explore(state);
        if out.cmd.valid {
            LAST_WINNER.store(3, Ordering::Relaxed);
            return out;
        }
    }

    // No layer produced output — return invalid
    BehaviorOutput {
        cmd: MotorOutput::none(),
        layer: 0xFF,
    }
}

/// Enable or disable a layer.  Layer 0 cannot be disabled.
pub fn layer_set_enabled(layer: usize, enabled: bool) {
    if layer == 0 || layer >= NUM_LAYERS { return; }
    LAYER_ENABLED[layer].store(enabled, Ordering::Relaxed);
}

/// Check if a layer is enabled.
pub fn layer_is_enabled(layer: usize) -> bool {
    if layer >= NUM_LAYERS { return false; }
    LAYER_ENABLED[layer].load(Ordering::Relaxed)
}

/// Return the index of the last winning layer (0xFF if none).
pub fn last_winner() -> u8 {
    LAST_WINNER.load(Ordering::Relaxed)
}

/// Return status of all layers.
pub fn layer_statuses() -> [LayerStatus; NUM_LAYERS] {
    let winner = LAST_WINNER.load(Ordering::Relaxed);
    let mut out = [LayerStatus { layer: 0, name: "", enabled: false, winning: false }; NUM_LAYERS];
    for i in 0..NUM_LAYERS {
        out[i] = LayerStatus {
            layer:   i as u8,
            name:    LAYER_NAMES[i],
            enabled: LAYER_ENABLED[i].load(Ordering::Relaxed),
            winning: winner == i as u8,
        };
    }
    out
}
