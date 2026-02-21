/// Halt the CPU until an interrupt arrives.
#[inline(always)]
pub fn wfi() {
    unsafe {
        core::arch::asm!("wfi");
    }
}

/// Read the hart ID from the tp register.
#[inline(always)]
pub fn hart_id() -> usize {
    let id: usize;
    unsafe {
        core::arch::asm!("mv {}, tp", out(reg) id);
    }
    id
}
