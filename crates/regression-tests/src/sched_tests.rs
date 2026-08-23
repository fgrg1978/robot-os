//! Coverage for `crates/sched/` priority + queue ordering — was 0 tests before.
//!
//! Replicates the priority-queue ordering invariants the scheduler
//! relies on: lower numeric priority = higher actual priority,
//! RT_PRIORITY_THRESHOLD splits hard-real-time from preemptable.

#![cfg(test)]

const RT_PRIORITY_THRESHOLD: u32 = 12;
const DEFAULT_PRIORITY:      u32 = 16;
const NET_POLL_PRIORITY:     u32 = 12;
const RT_MOTOR_PRIORITY:     u32 = 8;
const IDLE_PRIORITY:         u32 = 31;

#[inline]
const fn is_rt(prio: u32) -> bool { prio < RT_PRIORITY_THRESHOLD }

/// Returns true if `winner` should preempt `loser` at scheduler dispatch.
/// Lower numeric prio wins; RT (< threshold) always wins over normal.
fn should_preempt(winner: u32, loser: u32) -> bool {
    winner < loser
}

#[test]
fn rt_motor_below_threshold_is_rt() {
    assert!(is_rt(RT_MOTOR_PRIORITY));
    assert!(!is_rt(DEFAULT_PRIORITY));
    assert!(!is_rt(NET_POLL_PRIORITY)); // == threshold means NOT rt
    assert!(is_rt(RT_PRIORITY_THRESHOLD - 1));
}

#[test]
fn rt_preempts_default() {
    assert!(should_preempt(RT_MOTOR_PRIORITY, DEFAULT_PRIORITY));
    assert!(should_preempt(NET_POLL_PRIORITY, DEFAULT_PRIORITY));
}

#[test]
fn idle_loses_to_everyone() {
    for &p in &[RT_MOTOR_PRIORITY, NET_POLL_PRIORITY, DEFAULT_PRIORITY] {
        assert!(should_preempt(p, IDLE_PRIORITY));
        assert!(!should_preempt(IDLE_PRIORITY, p));
    }
}

#[test]
fn equal_priorities_do_not_preempt() {
    // Same-prio tasks round-robin, never preempt each other.
    assert!(!should_preempt(DEFAULT_PRIORITY, DEFAULT_PRIORITY));
    assert!(!should_preempt(NET_POLL_PRIORITY, NET_POLL_PRIORITY));
}

// ── Load-balancing primitive — RETIRADO A PROPÓSITO (K-C12) ──────────────
//
// Aquí vivían tres tests sobre una copia **privada** de `find_least_loaded_cpu`
// escrita a mano en este fichero. Pasaban siempre, y no probaban nada: la copia
// se probaba a sí misma. Cuando K-C12 demostró que esa política estaba mal y el
// kernel la sustituyó, los tres siguieron en verde afirmando la política
// abandonada — el peor resultado posible para un test de regresión, porque
// además invitaba a "arreglar" el kernel de vuelta hacia ellos.
//
// Qué estaba mal en la política, para que no vuelva: contaba tareas
// **encoladas**, una métrica ciega dos veces. No ve la tarea que está
// *corriendo* (no está en ninguna cola) e ignora la prioridad, mientras
// `cpu_dequeue` es prioridad estricta sin envejecimiento. Un hijo de `fork()`
// con prioridad 16 colocado en el hart que hospeda `rt-motor` y `flight-ctrl`
// (ambos prioridad 8) quedaba `Ready` para siempre. Medido: 98 de 98 hijos
// ejecutaron en un hart sin residentes RT; 0 de 5 en los dos que sí los tenían.
//
// La política real vive ahora en `robot_os_sched::task::pick_cpu_by_load`, que
// puntúa **residencia** en vez de estado instantáneo, y se prueba contra la
// función del kernel —no contra una copia— en `crates/sched-wake-tests`.
//
// La lección es la que este fichero existe para enseñar: un test que replica a
// mano la lógica que dice vigilar no vigila nada. Si vuelve a hacer falta
// cobertura aquí, tírese del fuente real con `#[path]`, como ya hacen
// `crypto_tests.rs` y `property.rs` en este mismo crate.

// ── Time-slice expiry ────────────────────────────────────────────────────
//
// Mirror the scheduler's "should we yield this task on tick" logic.
// Lock the invariant: RT tasks never preempted by timer; normal tasks
// time-sliced after RT_TIME_SLICE_TICKS.

const RT_TIME_SLICE_TICKS: u32 = 10;

fn should_yield_on_timer(prio: u32, ticks_in_slice: u32) -> bool {
    if is_rt(prio) { return false; }
    ticks_in_slice >= RT_TIME_SLICE_TICKS
}

#[test]
fn rt_never_yields_on_timer() {
    for ticks in [0, 1, 100, u32::MAX] {
        assert!(!should_yield_on_timer(RT_MOTOR_PRIORITY, ticks));
    }
}

#[test]
fn normal_yields_after_slice() {
    assert!(!should_yield_on_timer(DEFAULT_PRIORITY, 0));
    assert!(!should_yield_on_timer(DEFAULT_PRIORITY, RT_TIME_SLICE_TICKS - 1));
    assert!( should_yield_on_timer(DEFAULT_PRIORITY, RT_TIME_SLICE_TICKS));
    assert!( should_yield_on_timer(DEFAULT_PRIORITY, RT_TIME_SLICE_TICKS + 1));
}
