use bytemuck::{Pod, Zeroable};
use egui::ComboBox;
use egui_wgpu::ScreenDescriptor;
use glam::{Mat4, UVec2, UVec3, UVec4, Vec2, Vec3, Vec4};
use image::{Rgb32FImage, RgbImage};
use wgpu::{util::{DeviceExt, StagingBelt}, Buffer, BufferUsages};
use winit::window::Window;

use crate::{buffer::SSBO, egui_tools::EguiRenderer, shader::create_shader_module, simulation::{FluidSimulation, SimulationSettings, TickSettings}, uniform::Uniform};


const MSAA_SAMPLE_COUNT : u32 = 1;
const PARTICLE_COUNT : u32 = 100_000;
const SIZE : Vec2 = Vec2::new(53.0, 30.0);
pub const RENDER_DIMS : UVec2 = UVec2::new(1920/2, 1080/2);
pub const OBJECT_RENDER_TEXTURE_DIMS : UVec2 = UVec2::splat(512);


const QUAD_VERTICES : &[ParticleVertex] = &[
    ParticleVertex { pos: Vec2::new(1.0, 1.0) },
    ParticleVertex { pos: Vec2::new(1.0, 0.0) },
    ParticleVertex { pos: Vec2::new(0.0, 0.0) },
    ParticleVertex { pos: Vec2::new(0.0, 0.0) },
    ParticleVertex { pos: Vec2::new(0.0, 1.0) },
    ParticleVertex { pos: Vec2::new(1.0, 1.0) },

];


pub struct Renderer {
    pub simulation: FluidSimulation,

    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    surface: wgpu::Surface<'static>,
    pub window: &'static Window,

    staging_belt: StagingBelt,
    pub projection: Mat4,
    
    sim_settings: SimulationSettings,
    pub tick_settings: TickSettings,

    quad_vertices: Buffer,
    fluid_pipeline: FluidRenderPipeline,
    object_pipeline: ObjectRenderPipeline,

    pub egui: EguiRenderer,
}


pub struct FluidRenderPipeline {
    uniform: Uniform<Mat4>,
    render_pipeline: wgpu::RenderPipeline,
}


pub struct ObjectRenderPipeline {
    uniform: Uniform<ObjectUniform>,
    ssbo: SSBO<FluidObject>,
    render_pipeline: wgpu::RenderPipeline,
    objects: Vec<FluidObject>,
    output_texture: wgpu::Texture,
    staging_buffer: wgpu::Buffer,

    sender: std::sync::mpsc::Sender<Vec<Vec2>>,
    recv: std::sync::mpsc::Receiver<Vec<Vec2>>,
}


#[derive(Clone, Copy, Pod, Zeroable, Debug)]
#[repr(C)]
#[repr(align(16))]
struct ObjectUniform {
    inv_proj: Mat4,
    pad: Vec3,
    ssbo_len: u32,
}


#[derive(Clone, Copy, Pod, Zeroable, Debug)]
#[repr(C)]
#[repr(align(16))]
struct FluidObject {
    position: Vec2,
    kind: u32,
    pad: u32,
    pad2: UVec4,
}


#[derive(Clone, Copy, Pod, Zeroable, Debug)]
#[repr(C)]
struct ParticleVertex {
    pos: Vec2,
}


impl Renderer {
    pub async fn new(window: Window, settings: SimulationSettings) -> Self {
        let window = Box::leak(Box::new(window));
        let size = window.inner_size();

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::from_env_or_default());
        let surface = instance.create_surface(&*window).unwrap();

        let adapter = instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            }
        ).await.unwrap();

        let (device, queue) = adapter.request_device(
            &wgpu::DeviceDescriptor {
                required_features: wgpu::Features::empty()
                    | wgpu::Features::VERTEX_WRITABLE_STORAGE,
                required_limits: {
                    let mut limits = wgpu::Limits::downlevel_defaults();
                    limits.max_buffer_size = adapter.limits().max_buffer_size;
                    limits.max_compute_workgroups_per_dimension = adapter.limits().max_compute_workgroups_per_dimension;
                    dbg!(limits.max_compute_workgroups_per_dimension);
                    limits.max_storage_buffer_binding_size = adapter.limits().max_storage_buffer_binding_size;
                    limits.max_texture_dimension_2d = adapter.limits().max_texture_dimension_2d;
                    limits
                },
                label: Some("main device"),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            },
        ).await.unwrap();

        let surface_capabilities = surface.get_capabilities(&adapter);

        let surface_format = surface_capabilities.formats.iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(surface_capabilities.formats[0]);


        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::Immediate,
            alpha_mode: surface_capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&device, &config);

        let simulation = FluidSimulation::new(&device, settings);

        
        let fluid_pipeline = {
            let shader = create_shader_module(&device, wgpu::ShaderModuleDescriptor {
                label: Some("fluid-shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/fluid_shader.wgsl").into()),
            });


            let uniform = Uniform::new("fluid-shader-inv-proj", &device, 0, wgpu::ShaderStages::VERTEX_FRAGMENT);


            let rpl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("particle-render-pipeline-layout"),
                bind_group_layouts: &[simulation.simulation_settings_bgl(), simulation.simulation_bgl(), &uniform.bind_group_layout()],
                push_constant_ranges: &[],
            });


            let targets = [Some(wgpu::ColorTargetState {
                format: config.format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })];




            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("fluid-render-pipeline"),
                layout: Some(&rpl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[ParticleVertex::desc()],
                },


                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &targets,
                }),


                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },


                depth_stencil: None,


                multisample: wgpu::MultisampleState {
                    count: MSAA_SAMPLE_COUNT,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },


                multiview: None,
                cache: None,
            });


            FluidRenderPipeline {
                uniform,
                render_pipeline: pipeline,
            }
        };

        let object_pipeline = {
            let shader = create_shader_module(&device, wgpu::ShaderModuleDescriptor {
                label: Some("object-shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/image_shader.wgsl").into()),
            });


            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("object-pipeline-texture"),
                size: wgpu::Extent3d {
                    width: OBJECT_RENDER_TEXTURE_DIMS.x,
                    height: OBJECT_RENDER_TEXTURE_DIMS.y,
                    depth_or_array_layers: 1,
                },

                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Uint,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });


            let staging = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("object-pipeline-staging-buffer"),
                size: (OBJECT_RENDER_TEXTURE_DIMS.x * OBJECT_RENDER_TEXTURE_DIMS.y) as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });



            let uniform = Uniform::new("object-shader-ssbo-len", &device, 0, wgpu::ShaderStages::VERTEX_FRAGMENT);
            let ssbo : SSBO<FluidObject> = SSBO::new(
                "object-shader-ssbo", 
                &device, 
                BufferUsages::STORAGE | BufferUsages::COPY_SRC | BufferUsages::COPY_DST,
                wgpu::ShaderStages::VERTEX_FRAGMENT,
                128,
            );


            let rpl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("object-render-pipeline-layout"),
                bind_group_layouts: &[&uniform.bind_group_layout(), ssbo.layout()],
                push_constant_ranges: &[],
            });


            let targets = [Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::R8Uint,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })];



            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("object-render-pipeline"),
                layout: Some(&rpl),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[ParticleVertex::desc()],
                },


                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &targets,
                }),


                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    unclipped_depth: false,
                    conservative: false,
                },


                depth_stencil: None,


                multisample: wgpu::MultisampleState {
                    count: MSAA_SAMPLE_COUNT,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },


                multiview: None,
                cache: None,
            });


            let (sender, recv) = std::sync::mpsc::channel();
            ObjectRenderPipeline {
                uniform,
                ssbo,
                render_pipeline: pipeline,
                objects: vec![],
                output_texture: texture,
                staging_buffer: staging,
                sender,
                recv,
            }

        };



        let quad_vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-vertices"),
            contents: bytemuck::cast_slice(QUAD_VERTICES),
            usage: wgpu::BufferUsages::VERTEX,
        });





        let egui = EguiRenderer::new(
            &device,
            config.format,
            None,
            MSAA_SAMPLE_COUNT,
            window
        );


        let tick_settings = TickSettings {
            delta: 1.0 / 120.0,
            gravity: Vec2::ZERO,
            mass: 1.0,
            pressure_constant: 50.0,
            rest_density: 0.0,
            damping_factor: 0.1,
            viscosity_coefficient: 25.0,
            surface_tension_treshold: 0.1,
            surface_tension_coefficient: 35.0,
            mouse_force_radius: 5.0,
            mouse_force_power: 150.0,
            mouse_pos: Vec2::ZERO,
            mouse_state: 0,
        };


        object_pipeline.staging_buffer.slice(..)
            .map_async(wgpu::MapMode::Read, |_| ());


        Self {
            simulation,
            device,
            queue,
            config,
            window,
            surface,
            staging_belt: StagingBelt::new(1024 * 1024),
            projection: Mat4::from_scale(Vec3::splat(1.0)),
            tick_settings,
            sim_settings: settings,
            egui,
            fluid_pipeline,
            quad_vertices,
            object_pipeline,
        }
    }


    pub fn tick(&mut self, encoder: &mut wgpu::CommandEncoder) {
        self.simulation.tick(&self.queue, encoder, self.tick_settings);
    }



    pub fn render_fluid_to(&mut self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {

        self.fluid_pipeline.uniform.update(&self.queue, &self.projection.inverse());
        if !self.object_pipeline.objects.is_empty() {
            self.object_pipeline.ssbo.resize(&self.device, encoder, self.object_pipeline.objects.len());
            self.object_pipeline.ssbo.update(&mut self.staging_belt, encoder, &self.device, &self.object_pipeline.objects);
        }


        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("fluid-render-pass"),
            color_attachments: &[
                Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })
            ],

            depth_stencil_attachment: None,

            ..Default::default()
        });


        self.object_pipeline.uniform.update(&self.queue, &ObjectUniform {
            inv_proj: self.projection.inverse(),
            ssbo_len: self.object_pipeline.objects.len() as u32,
            pad: Vec3::ZERO,
        });


        pass.set_pipeline(&self.fluid_pipeline.render_pipeline);
        pass.set_bind_group(0, self.simulation.simulation_settings_bg(), &[]);
        pass.set_bind_group(1, self.simulation.simulation_bg(), &[]);
        pass.set_bind_group(2, &self.fluid_pipeline.uniform.bind_group, &[]);

        pass.set_vertex_buffer(0, self.quad_vertices.slice(..));

        pass.draw(0..(QUAD_VERTICES.len() as _), 0..1);

        drop(pass);


        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("object-render-pass"),
            color_attachments: &[
                Some(wgpu::RenderPassColorAttachment {
                    view: &self.object_pipeline.output_texture.create_view(&wgpu::wgt::TextureViewDescriptor::default()),
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })
            ],

            depth_stencil_attachment: None,

            ..Default::default()
        });


        pass.set_pipeline(&self.object_pipeline.render_pipeline);
        pass.set_bind_group(0, &self.object_pipeline.uniform.bind_group, &[]);
        pass.set_bind_group(1, self.object_pipeline.ssbo.bind_group(), &[]);

        pass.set_vertex_buffer(0, self.quad_vertices.slice(..));
        pass.draw(0..(QUAD_VERTICES.len() as _), 0..1);

        drop(pass);


        if self.simulation.tick > 10 {
        if let Ok(field) = self.object_pipeline.recv.recv() {
            self.queue.write_buffer(
                &self.simulation.force_field_texture(),
                0,
                bytemuck::cast_slice(&field),
            );

            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfoBase {
                    texture: &self.object_pipeline.output_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &self.object_pipeline.staging_buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(OBJECT_RENDER_TEXTURE_DIMS.x),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: OBJECT_RENDER_TEXTURE_DIMS.x,
                    height: OBJECT_RENDER_TEXTURE_DIMS.y,
                    depth_or_array_layers: 1,
                },
            );
        }
        }




        
        let buf = self.object_pipeline.staging_buffer.slice(..)
            .get_mapped_range();
        let slice : &[u8] = &*buf;
        let slice = slice.to_vec();
        let sender = self.object_pipeline.sender.clone();
        drop(buf);
        self.object_pipeline.staging_buffer.unmap();
        //self.object_pipeline.objects.clear();

        std::thread::spawn(move || {
            let img = Image::new(
                &slice,
                OBJECT_RENDER_TEXTURE_DIMS.x,
                OBJECT_RENDER_TEXTURE_DIMS.y,
            );
            let field = generate_smooth_gradient_field(img);

            sender.send(field).unwrap();
        });
        

    }



    pub fn render(&mut self, mut encoder: wgpu::CommandEncoder, egui: impl FnOnce(&egui::Context)) {
        let output = self.surface.get_current_texture().unwrap();
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());


        self.projection = glam::Mat4::orthographic_rh(
            -SIZE.x * 0.5, SIZE.x as f32 * 0.5,
            SIZE.y as f32 * 0.5, -SIZE.y * 0.5,
            -1.0, 0.0);

        self.render_fluid_to(&mut encoder, &view);

        let mut restart_sim = false;
        {

            let screen_descriptor = ScreenDescriptor {
                size_in_pixels: [self.config.width, self.config.height],
                pixels_per_point: self.window.scale_factor() as f32,
            };

            self.egui.begin_frame(self.window);


            egui(self.egui.context());


            egui::Window::new("spawn settings")
                .resizable(true)
                .vscroll(true)
                .default_open(false)
                .auto_sized()
                .show(self.egui.context(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label("particle count");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.sim_settings.particle_count)
                                .range(0..=u32::MAX)
                                .speed(10),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("particle spacing");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.sim_settings.particle_spacing)
                                .range(0.0..=f32::MAX)
                                .speed(0.025),
                        );
                    });


                    ui.horizontal(|ui| {
                        ui.label("smoothing radius");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.sim_settings.smoothing_radius)
                                .range(0.0..=f32::MAX)
                                .speed(0.025),
                        );
                    });


                    if ui.button("restart simulation").clicked() {
                        restart_sim = true;
                    }
                });

            egui::Window::new("simulation settings")
                .resizable(true)
                .vscroll(true)
                .default_open(false)
                .auto_sized()
                .show(self.egui.context(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label("delta");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.delta)
                                .range(0.0..=1.0)
                                .speed(0.001),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("gravity");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.gravity.x)
                                .range(0.0..=f32::MAX)
                                .speed(0.1),
                        );
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.gravity.y)
                                .range(0.0..=f32::MAX)
                                .speed(0.1),
                        );
                    });



                    ui.horizontal(|ui| {
                        ui.label("particle mass");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.mass)
                                .range(0.0..=f32::MAX)
                                .speed(0.025),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("pressure constant");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.pressure_constant)
                                .range(0.0..=f32::MAX)
                                .speed(0.025),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("rest density");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.rest_density)
                                .range(0.0..=f32::MAX)
                                .speed(0.025),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("damping factor");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.damping_factor)
                                .range(0.0..=f32::MAX)
                                .speed(0.025),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("viscosity coefficient");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.viscosity_coefficient)
                                .range(0.0..=f32::MAX)
                                .speed(0.025),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("surface tension treshold");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.surface_tension_treshold)
                                .speed(0.025),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("surface tension coefficient");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.surface_tension_coefficient)
                                .speed(0.025),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("mouse force radius");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.mouse_force_radius)
                                .range(0.0..=f32::MAX)
                                .speed(0.025),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("mouse force power");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.tick_settings.mouse_force_power)
                                .range(0.0..=f32::MAX)
                                .speed(0.025),
                        );
                    });
                });


            egui::Window::new("objects")
                .resizable(true)
                .vscroll(true)
                .default_open(false)
                .show(self.egui.context(), |ui| { ui.vertical(|ui| {
                    for (i, object) in self.object_pipeline.objects.iter_mut().enumerate() {
                        ui.label("object");

                        ui.horizontal(|ui| {
                            ui.label("position");
                            ui.add(
                                egui::widgets::DragValue::new(&mut object.position.x)
                                    .speed(0.1),
                            );
                            ui.add(
                                egui::widgets::DragValue::new(&mut object.position.y)
                                    .speed(0.1),
                            );
                        });


                        let current = match object.kind {
                            0 => "Circle",
                            1 => "Rect",
                            _ => unreachable!(),
                        };
                        ComboBox::new(i, current)
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut object.kind, 0, "Circle");
                                ui.selectable_value(&mut object.kind, 1, "Rect");
                            });

                        match object.kind {
                            0 => {
                                let mut radius = f32::from_ne_bytes(object.pad.to_ne_bytes());
                                // circle
                                ui.horizontal(|ui| {
                                    ui.label("Radius");
                                    ui.add(
                                        egui::widgets::DragValue::new(&mut radius)
                                            .speed(0.1)
                                            .range(0.0..=f32::INFINITY)
                                    );
                                });

                                object.pad = u32::from_ne_bytes(radius.to_ne_bytes());
                            },

                            1 => {
                                let mut rot = f32::from_ne_bytes(object.pad.to_ne_bytes());
                                let mut width = f32::from_ne_bytes(object.pad2.x.to_ne_bytes());
                                let mut height = f32::from_ne_bytes(object.pad2.y.to_ne_bytes());

                                ui.horizontal(|ui| {
                                    ui.label("Rotation");
                                    ui.add(
                                        egui::widgets::DragValue::new(&mut rot)
                                            .speed(0.1)
                                            .range(0.0..=f32::INFINITY)
                                    );
                                });


                                ui.horizontal(|ui| {
                                    ui.label("Extents");
                                    ui.add(
                                        egui::widgets::DragValue::new(&mut width)
                                            .speed(0.1),
                                    );
                                    ui.add(
                                        egui::widgets::DragValue::new(&mut height)
                                            .speed(0.1),
                                    );
                                });

                                object.pad = u32::from_ne_bytes(rot.to_ne_bytes());
                                object.pad2.x = u32::from_ne_bytes(width.to_ne_bytes());
                                object.pad2.y = u32::from_ne_bytes(height.to_ne_bytes());
                            }
                            _ => unreachable!(),
                        }


                        ui.separator();

                    }


                    if ui.button("Add").clicked() {
                        self.object_pipeline.objects.push(FluidObject {
                            position: Vec2::ZERO,
                            kind: 0,
                            pad: 0,
                            pad2: UVec4::ZERO,
                        });
                    }

                }) });

            
            self.egui.end_frame_and_draw(
                &self.device,
                &self.queue,
                &mut encoder,
                self.window,
                &view,
                screen_descriptor,
            );
        }



        self.staging_belt.finish();
        self.queue.submit(core::iter::once(encoder.finish()));
        self.staging_belt.recall();

        output.present();


        self.object_pipeline.staging_buffer.slice(..)
            .map_async(wgpu::MapMode::Read, |_| ());



        if restart_sim {
            self.restart_simulation();
        }
    }


    pub fn restart_simulation(&mut self) {
        self.simulation = FluidSimulation::new(&self.device, self.sim_settings)
    }


    pub fn draw_rect(&mut self, pos: Vec2, rot: f32, extents: Vec2) {
        self.object_pipeline.objects.push(FluidObject {
            position: pos,
            kind: 1,
            pad: u32::from_ne_bytes(rot.to_ne_bytes()),
            pad2: UVec4::new(
                u32::from_ne_bytes(extents.x.to_ne_bytes()),
                u32::from_ne_bytes(extents.y.to_ne_bytes()),
                0,
                0
            )
        });
    }


    pub fn draw_circle(&mut self, pos: Vec2, radius: f32) {
        self.object_pipeline.objects.push(FluidObject {
            position: pos,
            kind: 0,
            pad: u32::from_ne_bytes(radius.to_ne_bytes()),
            pad2: UVec4::ZERO,
        });
    }


    pub fn resize_surface(&mut self, width: u32, height: u32) {
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }
}



impl ParticleVertex {
    fn desc() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as _,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                }
            ],
        }
    }
}



struct Image<'a> {
    data: &'a [u8],
    width: u32,
    height: u32,
}


impl<'a> Image<'a> {
    pub fn new(data: &'a [u8], width: u32, height: u32) -> Self {
        Self {
            data,
            width,
            height,
        }
    }


    pub fn get_pixel(&self, x: u32, y: u32) -> u8 {
        self.data[(y*self.width+x) as usize]
    }
}



fn generate_smooth_gradient_field(img: Image) -> Vec<Vec2> {
    let height = img.height as usize;
    let width = img.width as usize;

    // Distance buffer, initialized to a large value
    let mut dist = vec![vec![f32::MAX; img.width as _]; img.height as _];
    // Nearest source pixel coords for each pixel
    let mut nearest = vec![vec![(0usize, 0usize); img.width as _]; img.height as usize];

    // Step 1: Initialization
    let mut has_white = false;
    for y in 0..height {
        for x in 0..width {
            if img.get_pixel(x as u32, y as u32) > 128 {
                dist[y][x] = 0.0;
                nearest[y][x] = (x, y);
                has_white = true;
            }

        }
    }

    if !has_white {
        for y in 0..height {
            for x in 0..width {
                if y == height-1 || y == 0 || x == width-1 || x == 0 {
                    dist[y][x] = 0.0;
                    nearest[y][x] = (x, y);
                }


            }
        }


    }


    // Helper to compute squared Euclidean distance
    let squared_dist = |x1, y1, x2, y2| -> f32 {
        let dx = x1 as f32 - x2 as f32;
        let dy = y1 as f32 - y2 as f32;
        dx*dx + dy*dy
    };

    // Step 2: Forward pass
    for y in 0..height {
        for x in 0..width {
            // Check neighbors: left, top-left, top, top-right
            for &(nx, ny) in &[
                (x.wrapping_sub(1), y), 
                (x.wrapping_sub(1), y.wrapping_sub(1)), 
                (x, y.wrapping_sub(1)), 
                (x + 1, y.wrapping_sub(1))
            ] {
                if nx < width && ny < height {
                    let candidate = nearest[ny][nx];
                    let candidate_dist = squared_dist(x, y, candidate.0, candidate.1);
                    if candidate_dist < dist[y][x] {
                        dist[y][x] = candidate_dist;
                        nearest[y][x] = candidate;
                    }
                }
            }
        }
    }

    // Step 3: Backward pass
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            // Check neighbors: right, bottom-right, bottom, bottom-left
            for &(nx, ny) in &[
                (x + 1, y), 
                (x + 1, y + 1), 
                (x, y + 1), 
                (x.wrapping_sub(1), y + 1)
            ] {
                if nx < width && ny < height {
                    let candidate = nearest[ny][nx];
                    let candidate_dist = squared_dist(x, y, candidate.0, candidate.1);
                    if candidate_dist < dist[y][x] {
                        dist[y][x] = candidate_dist;
                        nearest[y][x] = candidate;
                    }
                }
            }
        }
    }

    // Create RGB image for gradient visualization
    let mut gradient_field = vec![Vec2::ZERO; width * height];
    if !has_white {
        return gradient_field;
    }

    for y in 0..height {
        for x in 0..width {
            let (source_x, source_y) = nearest[y][x];

            // 1. REVERSE SUBTRACTION: Vector from source TO pixel (points away)
            let dir_x = x as f32 - source_x as f32;
            let dir_y = y as f32 - source_y as f32;

            // 2. NORMALIZE: Calculate the length and divide by it
            let length = (dir_x * dir_x + dir_y * dir_y).sqrt();

            let grad_x = if length > 1e-6 { dir_x } else { 0.0 };
            let grad_y = if length > 1e-6 { dir_y } else { 0.0 };

            gradient_field[y * width + x] = -Vec2::new(grad_x, grad_y);
        }
    }


    gradient_field
}

