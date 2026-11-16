use rune_display::retained_row_transaction::*;
use std::sync::{Arc, Barrier};
fn scope() -> RetainedRowScope {
    RetainedRowScope { sequence: 1, window: 1, buffer: 1, text_epoch: 1,
        source_start_scalar: 0, style_revision: 1, viewport_revision: 1,
        font_bank_generation: 1, shaping_policy_revision: 1 }
}
#[test]
fn cross_thread_observed_live_does_not_authorize_later_use() {
    for _ in 0..128 {
        let mut owner = RetainedRowTransaction::new().unwrap();
        let request = owner.begin(scope(), "AV").unwrap();
        let token = owner.complete(request.key(), 7, scope(), "AV").unwrap();
        let observed = Arc::new(Barrier::new(2));
        let revoked = Arc::new(Barrier::new(2));
        let worker = {
            let observed = observed.clone(); let revoked = revoked.clone();
            std::thread::spawn(move || {
                let stale_observation = token.is_live();
                observed.wait(); revoked.wait();
                assert!(stale_observation);
                assert!(!token.is_live());
                assert!(!request.is_live());
            })
        };
        observed.wait(); owner.cancel(); revoked.wait(); worker.join().unwrap();
    }
}
#[test]
fn retired_incarnation_clones_cannot_revoke_new_owner_lease() {
    let mut old = RetainedRowTransaction::new().unwrap();
    let old_request = old.begin(scope(), "AV").unwrap();
    let old_token = old.complete(old_request.key(), 7, scope(), "AV").unwrap();
    let worker = std::thread::spawn(move || { old.close(); old });
    let mut old = worker.join().unwrap();
    let mut fresh = RetainedRowTransaction::new().unwrap();
    let request = fresh.begin(scope(), "AV").unwrap();
    let token = fresh.complete(request.key(), 7, scope(), "AV").unwrap();
    assert_ne!(old_token.key(), token.key());
    assert!(!old_token.is_live()); assert!(!old_request.is_live());
    old.cancel(); drop(old); drop(old_token); drop(old_request);
    assert!(token.is_live()); assert!(request.is_live());
    drop(fresh);
    assert!(!token.is_live()); assert!(!request.is_live());
}
