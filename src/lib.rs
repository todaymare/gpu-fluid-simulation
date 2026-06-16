mod simulation;
mod buffer;
mod uniform;
mod renderer;
mod shader;
mod egui_tools;
mod input;

use std::time::Instant;
use glam::{Vec2, Vec4};
use rand::Rng;
use winit::{application::ApplicationHandler, dpi::LogicalSize, event::WindowEvent, event_loop::{ActiveEventLoop, ControlFlow, EventLoop}, window::{Window, WindowId}};
use crate::{input::InputManager, renderer::{Renderer, ObjectStore, OBJECT_RENDER_TEXTURE_DIMS}, simulation::SimulationSettings};



pub struct Engine {
    renderer: Renderer,
    input: InputManager,

    last_frame: Instant,
    time_since_simulation: f32,

    pos: Vec2,
    vel: Vec2,

    objects: ObjectStore,

    pipe_pos: Vec2,
    pipe_gap: f32,
}


impl Engine {
    pub fn run() {
        let event_loop = EventLoop::builder().build().unwrap();

        event_loop.set_control_flow(ControlFlow::Poll);

        event_loop.run_app(&mut EngineLauncher { engine: None }).unwrap();
    }


    pub fn redraw(&mut self) {
        self.renderer.device.poll(wgpu::PollType::Wait).unwrap();

        let elapsed = self.last_frame.elapsed();
        let dt = elapsed.as_secs_f32();
        self.last_frame = Instant::now();

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


        /*
        println!("{}", self.pipe_pos);

        self.renderer.draw_circle(self.pos, 1.0);
        let gap = self.pipe_gap;
        let mut pipe_pos = self.pipe_pos;
        pipe_pos.y += 15.0 + gap;
        self.renderer.draw_rect(pipe_pos, 0.0, Vec2::new(2.0, 30.0));
        let mut pipe_pos = self.pipe_pos;
        pipe_pos.y -= 15.0 + gap;
        self.renderer.draw_rect(pipe_pos, 0.0, Vec2::new(2.0, 30.0));
        */

        if self.pipe_pos.x < -(53.0 / 4.0) {
            self.pipe_gap += 3.5;
        }

        if self.pipe_pos.x < -(53.0 / 2.0) - 1.0 {
            self.pipe_pos.x = 53.0 / 2.0 + 1.0;
            self.pipe_pos.y = rand::rng().random_range(-10.0..10.0);
            self.pipe_gap = 5.0;
        }



        self.renderer.render(encoder, &mut self.objects, |ctx, _store| {
            egui::Window::new("Scene")
            .show(ctx, |ui| {

            });

        });

        if self.objects.load_image_pending {
            self.objects.load_image_pending = false;
            self.renderer.load_image(&mut self.objects);
        }

        self.renderer.window.request_redraw();
    }
}



struct EngineLauncher {
    engine: Option<Engine>,
}


impl ApplicationHandler for EngineLauncher {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = event_loop.create_window(Window::default_attributes().with_inner_size(LogicalSize::new(960, 540))).unwrap();

        let sim_settings = SimulationSettings {
            particle_count: 10_000,
            particle_spacing: 0.1,
            smoothing_radius: 1.0,
            size: Vec2::new(53.0, 30.0),
            texture_size: OBJECT_RENDER_TEXTURE_DIMS,

        };

        let mut renderer = pollster::block_on(Renderer::new(window, sim_settings));
        renderer.tick_settings.delta = 1.0 / 240.0;
        renderer.tick_settings.pressure_constant = 200.0;
        renderer.tick_settings.rest_density = 6.4;
        renderer.tick_settings.damping_factor = 0.7;
        renderer.tick_settings.viscosity_coefficient = 100.0;
        renderer.tick_settings.velocity_scale = 0.0055;
        renderer.tick_settings.velocity_log_factor = 5.0;

        self.engine = Some(Engine {
            renderer,
            input: InputManager::new(),
            last_frame: Instant::now(),
            time_since_simulation: 0.0,
            pos: Vec2::ZERO,
            vel: Vec2::ZERO,
            objects: ObjectStore::default(),
            pipe_pos: Vec2::ZERO,
            pipe_gap: 5.0,
        })
    }


    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(engine) = self.engine.as_mut()
        else { return };

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



            WindowEvent::MouseInput { device_id, state, button } => {
                let renderer = &mut engine.renderer;
                match (state, button) {
                    (winit::event::ElementState::Pressed, winit::event::MouseButton::Left) => renderer.tick_settings.mouse_state = -1,
                    (winit::event::ElementState::Pressed, winit::event::MouseButton::Right) => renderer.tick_settings.mouse_state = 1,
                    (winit::event::ElementState::Released, winit::event::MouseButton::Left) => renderer.tick_settings.mouse_state = 0,
                    (winit::event::ElementState::Released, winit::event::MouseButton::Right) => renderer.tick_settings.mouse_state = 0,
                    _ => (),
                }
            }



            WindowEvent::CursorMoved { device_id, position } => {
                let renderer = &mut engine.renderer;
                let position = Vec2::new(position.x as f32, position.y as f32);


                let window_size = renderer.window.inner_size();
                let window_size = Vec2::new(window_size.width as f32, window_size.height as f32);
                let ndc = (position / window_size) * 2.0 - 1.0;
                let clip_pos = Vec4::new(ndc.x, -ndc.y, 0.0, 1.0);
                let inv_proj = renderer.projection.inverse();

                let world_pos = inv_proj * clip_pos;
                let world_pos = world_pos.truncate() / world_pos.w;
                let world_pos = world_pos.truncate();

                renderer.tick_settings.mouse_pos = world_pos;
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
            }





            _ => (),
        }




    }
}



