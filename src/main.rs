use molasses::Engine;

fn main() {
    tracing_subscriber::fmt().init();
    Engine::run();
}
