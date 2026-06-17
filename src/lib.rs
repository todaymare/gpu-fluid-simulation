mod simulation;
mod buffer;
mod uniform;
mod renderer;
mod shader;
mod egui_tools;
mod input;
mod platform;
mod benchmark;

#[cfg(target_family = "wasm")]
mod load_image;

use std::pin::Pin;

use glam::{Vec2, Vec4};
use rand::Rng;
use winit::{application::ApplicationHandler, dpi::LogicalSize, event::WindowEvent, event_loop::{ActiveEventLoop, ControlFlow, EventLoop}, window::{Window, WindowId}};
use crate::{input::InputManager, renderer::{Renderer, ObjectStore, OBJECT_RENDER_TEXTURE_DIMS}, simulation::SimulationSettings};


#[cfg(target_family = "wasm")]
use wasm_bindgen::prelude::wasm_bindgen;


#[cfg(target_family = "wasm")]
#[wasm_bindgen(start)]
pub fn wasm_main() {
    // Belt-and-braces panic handler. console_error_panic_hook formats panics
    // with location info; the direct console::error_1 is a guaranteed fallback
    // in case the hook library itself or its formatting ever fails.
    std::panic::set_hook(Box::new(|info| {
        let msg = info.to_string();
        web_sys::console::error_1(&msg.into());
    }));
    console_error_panic_hook::set_once();

    web_sys::console::log_1(&"[molasses] wasm_main: start".into());

    run();
}


fn set_mouse_world_pos(renderer: &mut crate::renderer::Renderer, x: f64, y: f64) {
    let position = Vec2::new(x as f32, y as f32);
    let window_size = renderer.window.inner_size();
    let window_size = Vec2::new(window_size.width as f32, window_size.height as f32);
    let ndc = (position / window_size) * 2.0 - 1.0;
    let clip_pos = Vec4::new(ndc.x, -ndc.y, 0.0, 1.0);
    let inv_proj = renderer.projection.inverse();
    let world_pos = inv_proj * clip_pos;
    let world_pos = world_pos.truncate() / world_pos.w;
    renderer.tick_settings.mouse_pos = world_pos.truncate();
}


pub struct Engine {
    renderer: Renderer,
    input: InputManager,

    last_frame: platform::Instant,
    time_since_simulation: f32,

    pos: Vec2,
    vel: Vec2,

    objects: ObjectStore,

    pipe_pos: Vec2,
    pipe_gap: f32,
}


impl Engine {
    pub async fn new(window: &'static Window) -> Self {
        #[cfg(target_family = "wasm")]
        web_sys::console::log_1(&"[molasses] Engine::new: start".into());

        let sim_settings = SimulationSettings {
            particle_count: 20_000,
            particle_spacing: 0.1,
            smoothing_radius: 0.5,
            size: Vec2::new(53.0, 30.0),
            texture_size: OBJECT_RENDER_TEXTURE_DIMS,
        };

        let mut renderer = Renderer::new(window, sim_settings).await;
        #[cfg(target_family = "wasm")]
        web_sys::console::log_1(&"[molasses] Engine::new: Renderer::new complete".into());
        renderer.tick_settings.delta = 1.0 / 160.0;
        renderer.tick_settings.gravity = Vec2::new(0.0, 15.0);
        renderer.tick_settings.pressure_constant = 200.0;
        renderer.tick_settings.damping_factor = 0.2;
        renderer.tick_settings.viscosity_coefficient = 200.0;
        renderer.tick_settings.surface_tension_treshold = 2.0;
        renderer.tick_settings.surface_tension_coefficient = 0.2;
        renderer.tick_settings.mouse_force_radius = 5.0;
        renderer.tick_settings.mouse_force_power = 2.0;

        Self {
            renderer,
            input: InputManager::new(),
            last_frame: platform::Instant::now(),
            time_since_simulation: 0.0,
            pos: Vec2::ZERO,
            vel: Vec2::ZERO,
            objects: ObjectStore::default(),
            pipe_pos: Vec2::ZERO,
            pipe_gap: 5.0,
        }
    }


    pub fn redraw(&mut self) {
        #[cfg(target_family = "wasm")]
        web_sys::console::log_1(&"[molasses] redraw: start".into());

        self.renderer.device.poll(wgpu::PollType::Wait).unwrap();

        let elapsed = self.last_frame.elapsed();
        let dt = elapsed.as_secs_f32();
        self.last_frame = platform::Instant::now();

        self.time_since_simulation += dt;

        let mut encoder = self.renderer.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame-encoder"),
        });

        for i in 0..8 {
            self.renderer.tick(&mut encoder);
            if self.renderer.simulation.tick % 8 == 0 {

                self.vel += Vec2::new(0.0, 0.01);
                self.pos += self.vel;
                if self.input.is_key_just_pressed(winit::keyboard::KeyCode::Space) {
                    self.vel.y = -0.5;
                }
                self.pipe_pos.x -= 0.4;

                self.input.update();

            }
            self.time_since_simulation -= self.renderer.tick_settings.delta;
        }


        if self.pipe_pos.x < -(53.0 / 4.0) {
            self.pipe_gap += 3.5;
        }

        if self.pipe_pos.x < -(53.0 / 2.0) - 1.0 {
            self.pipe_pos.x = 53.0 / 2.0 + 1.0;
            self.pipe_pos.y = rand::rng().random_range(-10.0..10.0);
            self.pipe_gap = 5.0;
        }


        #[cfg(target_family = "wasm")]
        web_sys::console::log_1(&"[molasses] redraw: calling renderer.render".into());

        self.renderer.render(encoder, &mut self.objects, |ctx, _store| {

        });

        if self.objects.load_image_pending {
            self.objects.load_image_pending = false;
            self.renderer.load_image(&mut self.objects);
        }

        #[cfg(target_family = "wasm")]
        self.renderer.poll_loaded_image(&mut self.objects);

        self.renderer.window.request_redraw();

        #[cfg(target_family = "wasm")]
        web_sys::console::log_1(&"[molasses] redraw: end".into());
    }
}



enum AppState {
    Active(Engine),
    Initializing(Pin<Box<dyn Future<Output = Engine>>>, &'static Window),
    None,
}


struct App {
    app: AppState,
}


impl App {
    pub fn run() {
        let event_loop = EventLoop::builder().build().unwrap();

        event_loop.set_control_flow(ControlFlow::Poll);

        event_loop.run_app(&mut App {
            app: AppState::None,
        }).unwrap();
    }
}


pub fn run() {
    App::run();
}


impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !matches!(self.app, AppState::None) {
            return;
        }

        #[cfg(target_family = "wasm")]
        web_sys::console::log_1(&"[molasses] resumed: start".into());


        // Determine the initial window size. On WASM we use the canvas's
        // getBoundingClientRect (the actual laid-out CSS size, not just
        // window.innerWidth — which can be 0 if the canvas's parent hasn't
        // been laid out yet) and fall back to a safe default.
        let (mut w, mut h) = (960u32, 540u32);

        #[cfg(target_family = "wasm")]
        {
            use wasm_bindgen::JsCast;
            let canvas = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id("game-canvas"))
                .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok());

            if let Some(canvas) = canvas {
                let rect = canvas.get_bounding_client_rect();
                let cw = rect.width() as u32;
                let ch = rect.height() as u32;
                if cw > 0 && ch > 0 {
                    w = cw;
                    h = ch;
                }
            }

            web_sys::console::log_1(&format!("[molasses] resumed: initial size {w}x{h}").into());
        }


        #[cfg(not(target_family = "wasm"))]
        let window =
            Window::default_attributes()
            .with_inner_size(LogicalSize::new(w, h));

        #[cfg(target_family = "wasm")]
        let window = {
            use winit::platform::web::WindowAttributesExtWebSys;
            use wasm_bindgen::JsCast;

            let canvas =
                web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id("game-canvas"))
                .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok())
                .expect("Failed to find #game-canvas element");

            // Force the canvas dimensions so the surface never has a 0 size.
            // Use the rect-based w/h we computed above.
            canvas.set_width(w);
            canvas.set_height(h);

            // Don't pass inner_size — let CSS `width:100%;height:100%` fill the viewport.
            Window::default_attributes().with_canvas(Some(canvas))
        };

        let window = event_loop.create_window(window).unwrap();

        #[cfg(target_family = "wasm")]
        {
            web_sys::console::log_1(&"[molasses] resumed: window created".into());
            // Immediately size the canvas to the viewport — the initial
            // inner_size from `with_canvas` saw a stale layout rect.
            let w = web_sys::window().unwrap().inner_width().unwrap().as_f64().unwrap() as u32;
            let h = web_sys::window().unwrap().inner_height().unwrap().as_f64().unwrap() as u32;
            let _ = window.request_inner_size(LogicalSize::new(w, h));
        }

        let window = Box::leak(Box::new(window));
        let window = &*window;
        window.request_redraw();

        #[cfg(target_family = "wasm")]
        {
            use wasm_bindgen::closure::Closure;
            use wasm_bindgen::JsCast;

            let window_clone = window; // the winit window
            let closure = Closure::wrap(Box::new(move || {
                let w = web_sys::window().unwrap().inner_width().unwrap().as_f64().unwrap() as u32;
                let h = web_sys::window().unwrap().inner_height().unwrap().as_f64().unwrap() as u32;
                let _ = window_clone.request_inner_size(LogicalSize::new(w, h));
            }) as Box<dyn FnMut()>);

            web_sys::window()
                .unwrap()
                .add_event_listener_with_callback("resize", closure.as_ref().unchecked_ref())
                .unwrap();

            closure.forget();
        }


        let data = Box::pin(Engine::new(window));
        self.app = AppState::Initializing(data, window);

        #[cfg(target_family = "wasm")]
        web_sys::console::log_1(&"[molasses] resumed: Initializing".into());
    }


    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _: WindowId,
        event: WindowEvent,
    ) {
        let engine =
        match &mut self.app {
            AppState::Active(engine) => {
                engine
            },

            AppState::Initializing(pin, window) => {
                let waker = std::task::Waker::noop();
                let mut cx = std::task::Context::from_waker(&waker);

                window.request_redraw();

                if let std::task::Poll::Ready(mut data) = pin.as_mut().poll(&mut cx) {
                    #[cfg(target_family = "wasm")]
                    web_sys::console::log_1(&"[molasses] init: ready, switching to Active".into());
                    // Resize to the current window size — Resized events
                    // that arrived while Initializing were swallowed.
                    let size = data.renderer.window.inner_size();
                    data.renderer.resize_surface(size.width, size.height);

                    #[cfg(target_family = "wasm")]
                    {
                        use wasm_bindgen::JsCast;
                        if let Some(canvas) = web_sys::window()
                            .and_then(|w| w.document())
                            .and_then(|d| d.get_element_by_id("game-canvas"))
                            .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok())
                        {
                            canvas.set_width(size.width);
                            canvas.set_height(size.height);
                        }
                    }

                    data.renderer.window.request_redraw();
                    self.app = AppState::Active(data);
                }

                return;
            },
            AppState::None => {
                return;
            },
        };


        if engine
            .renderer
            .egui
            .handle_input(engine.renderer.window, &event)
            .consumed { return };


        match event {
            WindowEvent::RedrawRequested => {
                engine.redraw();

            },



            WindowEvent::CloseRequested => {
                event_loop.exit();
            },



            WindowEvent::MouseInput { device_id: _, state, button } => {
                let renderer = &mut engine.renderer;
                match (state, button) {
                    (winit::event::ElementState::Pressed, winit::event::MouseButton::Left) => renderer.tick_settings.mouse_state = -1,
                    (winit::event::ElementState::Pressed, winit::event::MouseButton::Right) => renderer.tick_settings.mouse_state = 1,
                    (winit::event::ElementState::Released, winit::event::MouseButton::Left) => renderer.tick_settings.mouse_state = 0,
                    (winit::event::ElementState::Released, winit::event::MouseButton::Right) => renderer.tick_settings.mouse_state = 0,
                    _ => (),
                }
            }



            WindowEvent::CursorMoved { device_id: _, position } => {
                set_mouse_world_pos(&mut engine.renderer, position.x, position.y);
            }


            WindowEvent::Touch(touch) => {
                let renderer = &mut engine.renderer;
                use winit::event::TouchPhase::*;
                match touch.phase {
                    Started | Moved => {
                        set_mouse_world_pos(renderer, touch.location.x, touch.location.y);
                        renderer.tick_settings.mouse_state = -1;
                    },
                    Ended | Cancelled => {
                        renderer.tick_settings.mouse_state = 0;
                    },
                }
            }



            WindowEvent::KeyboardInput { event, .. } => {
                match event.state {
                    winit::event::ElementState::Pressed => {
                        engine.input.set_pressed_key(event.physical_key);
                        if let Some(txt) = event.text {
                            for char in txt.chars() {
                                if char.is_ascii_control() {
                                    continue;
                                }
                                engine.input.new_char(char);
                            }
                        }
                    },
                    winit::event::ElementState::Released => engine.input.set_unpressed_key(event.physical_key),
                };


                if engine.input.is_key_pressed(winit::keyboard::KeyCode::ShiftLeft)
                    && engine.input.is_key_just_pressed(winit::keyboard::KeyCode::Escape) {
                    event_loop.exit();
                }
            }




            WindowEvent::Resized(v) => {
                engine.renderer.resize_surface(v.width, v.height);

                #[cfg(target_family = "wasm")]
                {
                    use wasm_bindgen::JsCast;
                    if let Some(canvas) = web_sys::window()
                        .and_then(|w| w.document())
                        .and_then(|d| d.get_element_by_id("game-canvas"))
                        .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok())
                    {
                        canvas.set_width(v.width);
                        canvas.set_height(v.height);
                    }
                }
            }




            _ => (),
        }




    }
}
