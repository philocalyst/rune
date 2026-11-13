use rune_display::{first_light_host::FirstLightHost, *};
use std::sync::Arc;
fn host() -> FirstLightHost {
    FirstLightHost::new(
        Arc::new(StringSnapshot::new("hello\nworld", 7)),
        WindowSpec::new(Rect { x: 16., y: 16., width: 168., height: 68. }),
        FixedMetrics::default(),
        Rect { x: 0., y: 0., width: 200., height: 100. },
        16.,
    )
    .unwrap()
}
#[test]
fn resize_relayouts_then_duplicate_is_idle_and_invalid_is_atomic() {
    let mut host = host();
    let before = host.frame();
    assert!(host.apply(&InputEvent::Resize { width: 80., height: 60., scale: 2. }).unwrap());
    let resized = host.frame();
    assert_eq!(resized.generation, 2);
    assert_eq!(resized.bounds.width, 80.);
    assert_eq!(resized.windows[0].viewport.width, 48.);
    assert_eq!(before.bounds.width, 200.);
    assert!(!host.apply(&InputEvent::Resize { width: 80., height: 60., scale: 2. }).unwrap());
    assert!(Arc::ptr_eq(&resized, &host.frame()));
    assert!(
        host.apply(&InputEvent::Resize { width: f32::NAN, height: 60., scale: 2. })
            .is_err()
    );
    assert!(Arc::ptr_eq(&resized, &host.frame()));
    assert!(host.apply(&InputEvent::Resize { width: 1., height: 1., scale: 1. }).unwrap());
    assert!(host.frame().windows[0].viewport.valid());
}
#[test]
fn pointer_places_revision_checked_cursor_and_rejects_foreign_hit() {
    let mut host = host();
    let frame = host.frame();
    let row = &frame.windows[0].rows[0];
    let hit = frame.hit_test(Point { x: row.clusters[2].bounds.x, y: row.bounds.y }).unwrap();
    assert!(host.apply(&InputEvent::PointerHit { hit, button: 1 }).unwrap());
    assert_eq!(host.frame().windows[0].cursor.as_ref().unwrap().position, 2);
    let frame = host.frame();
    let mut stale = hit;
    stale.revision += 1;
    assert!(host.apply(&InputEvent::PointerHit { hit: stale, button: 1 }).is_err());
    assert!(Arc::ptr_eq(&frame, &host.frame()));
    let mut foreign = hit;
    foreign.buffer_id += 1;
    assert!(host.apply(&InputEvent::PointerHit { hit: foreign, button: 1 }).is_err());
}
