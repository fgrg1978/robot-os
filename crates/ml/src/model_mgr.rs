//! Multi-model management — hot-swap, context-based selection (F19).
//!
//! Allows the kernel to maintain a registry of ML models, swap between them
//! at runtime without reboot, and select models based on context.

use core::sync::atomic::{AtomicU8, AtomicBool, Ordering};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of registered models.
pub const MAX_MODELS: usize = 8;

/// Maximum model name length in bytes.
pub const MODEL_NAME_MAX_LEN: usize = 32;

/// Maximum context tag length.
pub const CONTEXT_TAG_MAX_LEN: usize = 16;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Context tags for model selection.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ModelContext {
    /// Default / general purpose.
    Default  = 0,
    /// Indoor environment (no GPS, low light).
    Indoor   = 1,
    /// Outdoor environment (GPS, daylight).
    Outdoor  = 2,
    /// Night / low-light conditions.
    Night    = 3,
    /// High-speed operation (smaller, faster model).
    Fast     = 4,
    /// High-accuracy operation (larger, slower model).
    Accurate = 5,
}

/// A registered model entry.
#[derive(Clone, Copy)]
pub struct ModelEntry {
    /// Model name (e.g., "obstacle_det_indoor").
    pub name: [u8; MODEL_NAME_MAX_LEN],
    pub name_len: u8,
    /// FAT32 path to GGUF file.
    pub path: [u8; MODEL_NAME_MAX_LEN],
    pub path_len: u8,
    /// Context this model is best for.
    pub context: ModelContext,
    /// Whether this slot is active.
    pub active: bool,
    /// Whether this model is currently loaded in memory.
    pub loaded: bool,
}

impl ModelEntry {
    pub const fn empty() -> Self {
        Self {
            name: [0; MODEL_NAME_MAX_LEN],
            name_len: 0,
            path: [0; MODEL_NAME_MAX_LEN],
            path_len: 0,
            context: ModelContext::Default,
            active: false,
            loaded: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

/// Model registry.
static mut MODELS: [ModelEntry; MAX_MODELS] = {
    const EMPTY: ModelEntry = ModelEntry::empty();
    [EMPTY; MAX_MODELS]
};

/// Index of the currently active model (for inference).
static ACTIVE_MODEL: AtomicU8 = AtomicU8::new(0);

/// Whether any model is loaded and ready for inference.
static MODEL_READY: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Register a model in the registry. Returns model_id or None.
pub fn model_register(name: &[u8], path: &[u8], context: ModelContext) -> Option<u8> {
    if name.len() > MODEL_NAME_MAX_LEN || path.len() > MODEL_NAME_MAX_LEN {
        return None;
    }
    unsafe {
        for i in 0..MAX_MODELS {
            if !MODELS[i].active {
                let m = &mut MODELS[i];
                m.name[..name.len()].copy_from_slice(name);
                m.name_len = name.len() as u8;
                m.path[..path.len()].copy_from_slice(path);
                m.path_len = path.len() as u8;
                m.context = context;
                m.active = true;
                m.loaded = false;
                return Some(i as u8);
            }
        }
    }
    None
}

/// Unregister a model.
pub fn model_unregister(model_id: u8) {
    if (model_id as usize) >= MAX_MODELS { return; }
    unsafe { MODELS[model_id as usize] = ModelEntry::empty(); }
}

/// Select the best model for the given context.
/// Sets it as the active model for inference.
/// Returns the model_id or None if no suitable model found.
pub fn model_select_for_context(ctx: ModelContext) -> Option<u8> {
    unsafe {
        // First try exact context match
        for i in 0..MAX_MODELS {
            if MODELS[i].active && MODELS[i].context == ctx {
                ACTIVE_MODEL.store(i as u8, Ordering::Release);
                return Some(i as u8);
            }
        }
        // Fallback to Default context
        for i in 0..MAX_MODELS {
            if MODELS[i].active && MODELS[i].context == ModelContext::Default {
                ACTIVE_MODEL.store(i as u8, Ordering::Release);
                return Some(i as u8);
            }
        }
        // Fallback to any active model
        for i in 0..MAX_MODELS {
            if MODELS[i].active {
                ACTIVE_MODEL.store(i as u8, Ordering::Release);
                return Some(i as u8);
            }
        }
    }
    None
}

/// Get the currently active model ID.
pub fn model_active_id() -> u8 {
    ACTIVE_MODEL.load(Ordering::Acquire)
}

/// Get info about a registered model.
pub fn model_info(model_id: u8) -> Option<(u8, u8, ModelContext, bool)> {
    if (model_id as usize) >= MAX_MODELS { return None; }
    unsafe {
        let m = &MODELS[model_id as usize];
        if !m.active { return None; }
        Some((m.name_len, m.path_len, m.context, m.loaded))
    }
}

/// Mark a model as loaded (after reading GGUF file into memory).
pub fn model_set_loaded(model_id: u8, loaded: bool) {
    if (model_id as usize) >= MAX_MODELS { return; }
    unsafe {
        MODELS[model_id as usize].loaded = loaded;
    }
    if loaded {
        MODEL_READY.store(true, Ordering::Release);
    }
}

/// Check if the active model is ready for inference.
pub fn model_is_ready() -> bool {
    MODEL_READY.load(Ordering::Acquire)
}

/// Count registered models.
pub fn model_count() -> usize {
    let mut count = 0;
    unsafe {
        for i in 0..MAX_MODELS {
            if MODELS[i].active { count += 1; }
        }
    }
    count
}
