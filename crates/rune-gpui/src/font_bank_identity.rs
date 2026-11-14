//! UI font-bank correlation IDs, never input/security authority.
static NEXT_BANK_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn allocate_bank_id(counter: &std::sync::atomic::AtomicU64) -> Result<u64, &'static str> {
    use std::sync::atomic::Ordering;
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| "native font bank identity exhausted")
}
pub(super) fn next_bank_id() -> Result<u64, &'static str> {
    allocate_bank_id(&NEXT_BANK_ID)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn correlation_ids_are_unique_and_exhaustion_never_wraps() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let counter = AtomicU64::new(1);
        assert_eq!(allocate_bank_id(&counter), Ok(1));
        assert_eq!(allocate_bank_id(&counter), Ok(2));
        let counter = AtomicU64::new(u64::MAX);
        assert!(allocate_bank_id(&counter).is_err());
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}
