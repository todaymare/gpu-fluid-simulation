use molasses::run;

fn main() {
    #[cfg(not(target_family = "wasm"))]
    {
        tracing_subscriber::fmt().init();
    }

    #[cfg(target_family = "wasm")]
    {
        let _ = console_log::init_with_level(log::Level::Info);
    }

    run();
}
