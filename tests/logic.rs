use active_desktop::core::renderer::cover_rect;
use active_desktop::core::workerw::can_cover;

#[test]
fn rejects_stub_workerw() {
    assert!(!can_cover(166, 47));
    assert!(!can_cover(639, 1080));
    assert!(can_cover(1920, 1080));
}

#[test]
fn cover_same_aspect_is_full() {
    let (u0, v0, u1, v1) = cover_rect((1920, 1080), (1920, 1080));
    assert!((u0 - 0.0).abs() < 1e-3 && (u1 - 1.0).abs() < 1e-3);
    assert!((v0 - 0.0).abs() < 1e-3 && (v1 - 1.0).abs() < 1e-3);
}

#[test]
fn cover_wider_destination_crops_vertically() {
    // 1920x1080 -> 2560x1080: scale to width, crop top/bottom, full u range.
    let (u0, v0, u1, v1) = cover_rect((1920, 1080), (2560, 1080));
    assert!((u0 - 0.0).abs() < 1e-3 && (u1 - 1.0).abs() < 1e-3);
    assert!(v0 > 0.0 && v1 < 1.0);
    assert!(((v0 + v1) - 1.0).abs() < 1e-3, "crop must be centred");
}

#[test]
fn cover_taller_destination_crops_horizontally() {
    // 1920x1080 -> 1080x1920: scale to height, crop left/right, full v range.
    let (u0, v0, u1, v1) = cover_rect((1920, 1080), (1080, 1920));
    assert!((v0 - 0.0).abs() < 1e-3 && (v1 - 1.0).abs() < 1e-3);
    assert!(u0 > 0.0 && u1 < 1.0);
    assert!(((u0 + u1) - 1.0).abs() < 1e-3, "crop must be centred");
}
