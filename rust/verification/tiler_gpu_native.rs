//! Actual guest twelve-worker tiler output -> existing CPU untile -> Metal blit.
//! Source is a deterministic test pattern, not a game frame. No GPU readback.
#[allow(dead_code)]
#[path = "tiler_threads_native.rs"]
mod tiler_threads_native;
use std::path::Path;
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    let frame = tiler_threads_native::produce(file, archive, directory)?;
    let rgba = native_ps5::video_tile::untile_rgba(&frame.tiled, frame.width, frame.height)?;
    assert_eq!(
        rgba, frame.expected_rgba,
        "Every pixel supplied to Metal must match source"
    );
    let mut presenter = native_ps5::metal_present::Presenter::new(frame.width, frame.height)?;
    let outcome = presenter.present(&rgba);
    let close = presenter.close();
    let evidence = outcome?;
    close?;
    assert!(!evidence.device_name.is_empty());
    assert!(evidence.presented_time.is_finite() && evidence.presented_time > 0.0);
    println!(
        "PASS actual guest twelve-worker tiler -> CPU untile ({} reference pixels) -> Metal drawable: {:?}; deterministic input, no GPU readback, no game loop",
        frame.width * frame.height,
        evidence
    );
    Ok(())
}
