use std::sync::{
    Once,
    atomic::{AtomicBool, Ordering},
};

static ENABLED: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK: Once = Once::new();

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = log)]
    fn console_log(message: &str);
}

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if ENABLED.load(Ordering::Relaxed) {
                if let Some(location) = info.location() {
                    event(&format!(
                        "Rust panic at {}:{}:{}",
                        location.file(),
                        location.line(),
                        location.column()
                    ));
                } else {
                    event("Rust panic (source location unavailable)");
                }
            } else {
                previous(info);
            }
        }));
    });
}

pub fn event(message: &str) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let message = format!("[monero-wasm] {message}");
    #[cfg(target_arch = "wasm32")]
    console_log(&message);
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("{message}");
}
