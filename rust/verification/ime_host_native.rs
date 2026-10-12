//! Synthetic lifecycle probe only. Does not type, confirm, or collect user input.
use native_ps5::{
    ime_dialog::{Backend, Completion, Request},
    ime_host::AppKitBackend,
    metal_present::Presenter,
};
fn main() -> Result<(), String> {
    let mut presenter = Presenter::new(640, 360)?;
    let mut backend = AppKitBackend::new(&presenter)?;
    assert!(backend.available());
    let request = Request {
        user: 1,
        title: "Synthetic IME lifecycle check".encode_utf16().collect(),
        initial_text: "No input required".encode_utf16().collect(),
        capacity_units: 128,
    };
    backend
        .begin(&request)
        .map_err(|e| format!("first begin: {e}"))?;
    assert!(
        backend.poll()?.is_none(),
        "No automatic acceptance or cancellation"
    );
    assert!(backend.begin(&request).is_err());
    backend.close()?;
    backend.close()?;
    presenter.pump(std::time::Duration::from_millis(100))?;
    backend
        .begin(&request)
        .map_err(|e| format!("second begin: {e}"))?;
    presenter.close()?;
    assert_eq!(backend.poll()?, Some(Completion::Aborted));
    backend.close()?;
    assert!(
        backend.begin(&request).is_err(),
        "Closed parent must reject a new sheet"
    );
    println!(
        "PASS: configured IME sheet create/pending/close/reopen/parent-close abort; no accepted input or real-user completion tested"
    );
    Ok(())
}
