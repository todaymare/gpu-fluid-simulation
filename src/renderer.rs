pub mod textures;

use bytemuck::{Pod, Zeroable};
use egui::ComboBox;
use egui_wgpu::ScreenDescriptor;
use glam::{Mat4, UVec2, Vec2, Vec3, Vec3Swizzles, Vec4};
use sti::{key::Key, vec::KVec};
use wgpu::{util::{DeviceExt, StagingBelt}, Buffer};
use winit::window::Window;

use crate::{buffer::ResizableBuffer, egui_tools::EguiRenderer, renderer::textures::{AtlasManager, Texture, TextureAtlasId}, shader::create_shader_module, simulation::{FluidSimulation, RenderSettings, SimulationSettings, TickSettings}, uniform::Uniform};


const MSAA_SAMPLE_COUNT : u32 = 1;
const PARTICLE_COUNT : u32 = 100_000;
const SIZE : Vec2 = Vec2::new(53.0, 30.0);
pub const RENDER_DIMS : UVec2 = UVec2::new(1920/2, 1080/2);
pub const OBJECT_RENDER_TEXTURE_DIMS : UVec2 = UVec2::splat(512);


// Vertices in [0, 1] used by the fluid pipeline. The fluid shader does
// `(position - 0.5) * 2.0` to map to NDC, so this range becomes [-1, 1].
const FLUID_QUAD_VERTICES : &[ParticleVertex] = &[
    ParticleVertex { pos: Vec2::new(1.0, 1.0) },
    ParticleVertex { pos: Vec2::new(1.0, 0.0) },
    ParticleVertex { pos: Vec2::new(0.0, 0.0) },
    ParticleVertex { pos: Vec2::new(0.0, 0.0) },
    ParticleVertex { pos: Vec2::new(0.0, 1.0) },
    ParticleVertex { pos: Vec2::new(1.0, 1.0) },
];

// Vertices in [-0.5, +0.5] used by the object pipeline. The new shader's
// `make_transform_2d_mat4` scales by the instance scale, so this range
// means "a 1x1 quad" in world units.
const OBJECT_QUAD_VERTICES : &[ParticleVertex] = &[
    ParticleVertex { pos: Vec2::new(0.5, 0.5) },
    ParticleVertex { pos: Vec2::new(0.5, -0.5) },
    ParticleVertex { pos: Vec2::new(-0.5, -0.5) },
    ParticleVertex { pos: Vec2::new(-0.5, -0.5) },
    ParticleVertex { pos: Vec2::new(-0.5, 0.5) },
    ParticleVertex { pos: Vec2::new(0.5, 0.5) },
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
    pub render_settings: RenderSettings,

    quad_vertices: Buffer,
    fluid_pipeline: FluidRenderPipeline,
    object_pipeline: ObjectRenderPipeline,

    pub atlas_manager: AtlasManager,

    pub egui: EguiRenderer,
}


pub struct FluidRenderPipeline {
    uniform: Uniform<Mat4>,
    render_pipeline: wgpu::RenderPipeline,
}


pub struct ObjectRenderPipeline {
    uniform: Uniform<ObjectUniform>,
    render_pipeline: wgpu::RenderPipeline,
    quad_vertices: Buffer,
    instances: ResizableBuffer<QuadInstance>,

    output_texture: wgpu::Texture,
    staging_buffer: wgpu::Buffer,

    sender: std::sync::mpsc::Sender<Vec<Vec2>>,
    recv: std::sync::mpsc::Receiver<Vec<Vec2>>,

    // Set when the GPU has finished copying the previous frame's object texture
    // into the staging buffer and the async map callback has fired. We poll
    // this at the start of each `render_fluid_to` so the WebGPU backend
    // (where `device.poll(Wait)` doesn't synchronously drive map callbacks)
    // sees a fully-mapped buffer before we call `get_mapped_range`.
    map_complete: std::sync::Arc<std::sync::Mutex<bool>>,

    // Tracks whether the staging buffer currently has a map_async in flight
    // (and therefore should not be mapped again until it is unmapped).
    buffer_mapped: bool,
}


#[derive(Clone, Copy, Pod, Zeroable, Debug)]
#[repr(C)]
#[repr(align(16))]
struct ObjectUniform {
    proj: Mat4,
    pad: Vec3,
    treshold: f32,
}


#[derive(Debug, Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct QuadInstance {
    pub colour: Vec4,
    pub uv    : Vec4,
    pub pos   : Vec2,
    pub scale : Vec2,
    pub rot   : f32,
    pub z     : f32,
    pub kind  : u32,
    pub pad   : f32,
}


#[derive(Debug, Clone, Copy)]
pub struct Quad {
    pub pos: Vec3,
    pub scale: Vec2,
    pub rot: f32,
    pub colour: Vec4,
    pub texture: Texture,
}


pub struct ObjectStore {
    pub quads: Vec<Quad>,
    pub threshold: f32,
    pub load_image_pending: bool,
}


impl Default for ObjectStore {
    fn default() -> Self {
        Self {
            quads: Vec::new(),
            threshold: 0.5,
            load_image_pending: false,
        }
    }
}




#[derive(Clone, Copy, Pod, Zeroable, Debug)]
#[repr(C)]
struct ParticleVertex {
    pos: Vec2,
}


impl Renderer {
    pub async fn new(window: &'static Window, settings: SimulationSettings) -> Self {
        let size = window.inner_size();
        // wgpu refuses a 0x0 surface texture. If the window is briefly 0
        // (some web backends report the canvas's internal buffer before
        // the CSS layout is committed) substitute a 1x1 placeholder; the
        // real size will be applied on the next Resized event.
        let size = winit::dpi::PhysicalSize::new(size.width.max(1), size.height.max(1));

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            #[cfg(target_arch = "wasm32")]
            backends: wgpu::Backends::BROWSER_WEBGPU,
            #[cfg(not(target_arch = "wasm32"))]
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let surface = instance.create_surface(window).unwrap();

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
            .unwrap_or_else(|| {
                #[cfg(target_arch = "wasm32")]
                web_sys::console::log_1(&"[molasses] WARNING: no sRGB surface format available".into());
                surface_capabilities.formats[0]
            });

        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&format!("[molasses] surface format: {surface_format:?}").into());


        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width,
            height: size.height,
            // WebGPU only supports FIFO/Auto*. Use FIFO on web, Immediate elsewhere
            // for the lowest-latency frame presentation.
            present_mode: {
                #[cfg(target_arch = "wasm32")]
                { wgpu::PresentMode::Fifo }
                #[cfg(not(target_arch = "wasm32"))]
                { wgpu::PresentMode::Immediate }
            },
            alpha_mode: surface_capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&device, &config);

        let simulation = FluidSimulation::new(&device, settings);

        let atlas_manager = AtlasManager::new(&device, &queue);

        
        let fluid_pipeline = {
            let shader = create_shader_module(&device, wgpu::ShaderModuleDescriptor {
                label: Some("fluid-shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/fluid_shader.wgsl").into()),
            });


            let uniform = Uniform::new("fluid-shader-inv-proj", &device, 0, wgpu::ShaderStages::VERTEX_FRAGMENT);


            let rpl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("particle-render-pipeline-layout"),
                bind_group_layouts: &[simulation.simulation_settings_bgl(), simulation.render_bgl(), &uniform.bind_group_layout()],
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


            let uniform = Uniform::new("object-shader-uniform", &device, 0, wgpu::ShaderStages::VERTEX_FRAGMENT);


            let rpl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("object-render-pipeline-layout"),
                bind_group_layouts: &[uniform.bind_group_layout(), &atlas_manager.bgl],
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
                    buffers: &[ParticleVertex::desc(), QuadInstance::desc()],
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


            let quad_vertices_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("object-quad-vertices"),
                contents: bytemuck::cast_slice(OBJECT_QUAD_VERTICES),
                usage: wgpu::BufferUsages::VERTEX,
            });

            let instances = ResizableBuffer::new(
                "object-instance-buffer",
                &device,
                wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                1024,
            );

            let (sender, recv) = std::sync::mpsc::channel();
            ObjectRenderPipeline {
                uniform,
                render_pipeline: pipeline,
                quad_vertices: quad_vertices_buf,
                instances,
                output_texture: texture,
                staging_buffer: staging,
                sender,
                recv,
                map_complete: std::sync::Arc::new(std::sync::Mutex::new(false)),
                buffer_mapped: false,
            }

        };



        let quad_vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("fluid-quad-vertices"),
            contents: bytemuck::cast_slice(FLUID_QUAD_VERTICES),
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
            surface_tension_treshold: 1.0,
            surface_tension_coefficient: 0.0,
            mouse_force_radius: 5.0,
            mouse_force_power: 150.0,
            mouse_pos: Vec2::ZERO,
            mouse_state: 0,
        };


        let render_settings = RenderSettings {
            density_scale: 0.01,
            density_log_factor: 5.0,
            show_force_field: false,
            render_smoothing: 0.06,
            render_base_color: Vec4::new(0.4, 0.7, 1.0, 1.0),
            render_lerp_color: Vec4::new(1.0, 1.0, 1.0, 1.0),
            max_render_density: 30.0,
            render_saturation_color: Vec4::new(1.0, 1.0, 1.0, 1.0),
            render_edge_color: Vec4::new(1.0, 1.0, 1.0, 1.0),
            edge_distance: 0.4,
        };


        // The first map of the staging buffer is requested at the end of
        // the first `render` call (see below). The first `render_fluid_to`
        // will skip the read because the map hasn't completed yet -- the
        // data is garbage anyway since no `copy_texture_to_buffer` was
        // issued for this frame.


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
            render_settings,
            sim_settings: settings,
            egui,
            fluid_pipeline,
            quad_vertices,
            object_pipeline,
            atlas_manager,
        }
    }


    pub fn tick(&mut self, encoder: &mut wgpu::CommandEncoder) {
        self.simulation.tick(&self.queue, encoder, self.tick_settings, self.render_settings);
    }



    pub fn render_fluid_to(&mut self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, store: &ObjectStore) {

        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&"[molasses] render_fluid_to: start".into());

        self.fluid_pipeline.uniform.update(&self.queue, &self.projection.inverse());


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


        pass.set_pipeline(&self.fluid_pipeline.render_pipeline);
        pass.set_bind_group(0, self.simulation.simulation_settings_bg(), &[]);
        pass.set_bind_group(1, self.simulation.render_bg(), &[]);
        pass.set_bind_group(2, &self.fluid_pipeline.uniform.bind_group, &[]);

        pass.set_vertex_buffer(0, self.quad_vertices.slice(..));

        pass.draw(0..(FLUID_QUAD_VERTICES.len() as _), 0..1);

        drop(pass);


        // Bucket quads by atlas id, like ravioli does. Each bucket becomes
        // a contiguous slice of the uploaded instance buffer and a single
        // draw call with its own atlas bind group.
        let mut buckets: KVec<TextureAtlasId, Vec<QuadInstance>> = KVec::new();
        for q in &store.quads {
            let atlas_id = q.texture.0;
            if buckets.len() <= atlas_id.usize() {
                buckets.resize(atlas_id.usize() + 1, Vec::new());
            }
            buckets[atlas_id].push(QuadInstance {
                colour: q.colour,
                uv: self.atlas_manager.get_uv(q.texture),
                pos: q.pos.xy(),
                scale: q.scale,
                rot: q.rot,
                z: q.pos.z,
                kind: 0,
                pad: 0.0,
            });
        }

        // Flatten into a single contiguous upload, recording each bucket's
        // offset range so the draw loop can issue one `draw` per atlas.
        let mut flat: Vec<QuadInstance> = Vec::new();
        let mut ranges: Vec<(TextureAtlasId, std::ops::Range<u32>)> = Vec::new();
        for (atlas_id, bucket) in buckets.into_iter() {
            if bucket.is_empty() { continue; }
            let start = flat.len() as u32;
            flat.extend(bucket);
            let end = flat.len() as u32;
            ranges.push((atlas_id, start..end));
        }

        self.object_pipeline.instances.resize(&self.device, encoder, flat.len());
        self.object_pipeline.instances.write(&mut self.staging_belt, encoder, &self.device, 0, &flat);

        self.object_pipeline.uniform.update(&self.queue, &ObjectUniform {
            proj: self.projection,
            treshold: store.threshold,
            pad: Vec3::ZERO,
        });


        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("object-render-pass"),
            color_attachments: &[
                Some(wgpu::RenderPassColorAttachment {
                    view: &self.object_pipeline.output_texture.create_view(&wgpu::wgt::TextureViewDescriptor::default()),
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 1.0, g: 0.0, b: 0.0, a: 0.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })
            ],

            depth_stencil_attachment: None,

            ..Default::default()
        });


        pass.set_pipeline(&self.object_pipeline.render_pipeline);
        pass.set_bind_group(0, &self.object_pipeline.uniform.bind_group, &[]);
        pass.set_vertex_buffer(0, self.object_pipeline.quad_vertices.slice(..));
        pass.set_vertex_buffer(1, self.object_pipeline.instances.buffer.slice(..));

        for (atlas_id, range) in &ranges {
            pass.set_bind_group(1, self.atlas_manager.get_bg(*atlas_id), &[]);
            pass.draw(
                0..(OBJECT_QUAD_VERTICES.len() as _),
                range.clone(),
            );
        }

        drop(pass);


        if self.simulation.tick > 10 {
            if let Ok(field) = self.object_pipeline.recv.try_recv() {
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




        // Read the staging buffer that was mapped at the end of the
        // previous `render`. If the map hasn't completed yet (first frame
        // or the WebGPU callback hasn't fired) skip the read and the
        // unmap -- the buffer wasn't written to anyway.
        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&"[molasses] render_fluid_to: checking map flag".into());
        if self.staging_buffer_ready() {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&"[molasses] render_fluid_to: map ready, reading".into());
            let buf = self.object_pipeline.staging_buffer.slice(..)
                .get_mapped_range();
            let slice : &[u8] = &*buf;
            let slice = slice.to_vec();
            drop(buf);
            self.object_pipeline.staging_buffer.unmap();
            self.object_pipeline.buffer_mapped = false;

            let img = Image::new(
                &slice,
                OBJECT_RENDER_TEXTURE_DIMS.x,
                OBJECT_RENDER_TEXTURE_DIMS.y,
            );
            let field = generate_smooth_gradient_field(img);

            #[cfg(not(target_arch = "wasm32"))]
            {
                let sender = self.object_pipeline.sender.clone();
                std::thread::spawn(move || {
                    sender.send(field).unwrap();
                });
            }

            #[cfg(target_arch = "wasm32")]
            {
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
        } else {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&"[molasses] render_fluid_to: map not ready, skipping read".into());
        }

    }


    pub fn load_image(&mut self, store: &mut ObjectStore) {
        #[cfg(not(target_family = "wasm"))]
        {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("image", &["png", "jpg", "jpeg", "bmp", "gif", "tga", "webp"])
                .pick_file()
                else { return };

            let Ok(bytes) = std::fs::read(&path) else { return };
            let Ok(img) = image::load_from_memory(&bytes) else { return };
            let rgba = img.to_rgba8();
            let (w, h) = (rgba.width() as f32, rgba.height() as f32);
            if w == 0.0 || h == 0.0 { return; }

            let texture = self.atlas_manager.register_image(&self.device, &self.queue, &rgba);

            // Fit into a 4x4 world-unit box, preserving aspect ratio.
            let (scale_x, scale_y) = if w >= h { (4.0, 4.0 * h / w) } else { (4.0 * w / h, 4.0) };

            store.quads.push(Quad {
                pos: Vec3::ZERO,
                scale: Vec2::new(scale_x, scale_y),
                rot: 0.0,
                colour: Vec4::ONE,
                texture,
            });
        }
    }


    pub fn render(&mut self, mut encoder: wgpu::CommandEncoder, store: &mut ObjectStore, egui: impl FnOnce(&egui::Context, &mut ObjectStore)) {
        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&"[molasses] render: get_current_texture".into());

        let output = self.surface.get_current_texture().unwrap();
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());

        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&"[molasses] render: got texture".into());


        let aspect = self.config.width as f32 / self.config.height as f32;
        let vert = SIZE.y;
        let horz = vert * aspect;
        let bounds = Vec2::new(horz, vert);

        // Recreate the simulation's spatial grid if the bounds have grown
        // enough to change the cell count.
        if bounds != self.sim_settings.size {
            self.sim_settings.size = bounds;
            self.simulation.resize(&self.device, bounds);
        }

        self.projection = glam::Mat4::orthographic_rh(
            -bounds.x * 0.5, bounds.x * 0.5,
            bounds.y * 0.5, -bounds.y * 0.5,
            -1.0, 0.0);

        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&"[molasses] render: pre render_fluid_to".into());

        self.render_fluid_to(&mut encoder, &view, store);

        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&"[molasses] render: post render_fluid_to".into());

        let mut restart_sim = false;
        {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&"[molasses] render: pre egui".into());

            let screen_descriptor = ScreenDescriptor {
                size_in_pixels: [self.config.width, self.config.height],
                pixels_per_point: self.window.scale_factor() as f32,
            };

            self.egui.begin_frame(self.window);


            egui(self.egui.context(), store);


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


            egui::Window::new("rendering")
                .resizable(true)
                .vscroll(true)
                .default_open(false)
                .auto_sized()
                .show(self.egui.context(), |ui| {
                    ui.horizontal(|ui| {
                        ui.label("density scale");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.render_settings.density_scale)
                                .range(0.0..=f32::MAX)
                                .speed(0.0001),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("density log factor");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.render_settings.density_log_factor)
                                .range(0.001..=100.0)
                                .speed(0.05),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("render smoothing");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.render_settings.render_smoothing)
                                .range(0.0001..=f32::MAX)
                                .speed(0.005),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("max render density");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.render_settings.max_render_density)
                                .range(0.0..=f32::MAX)
                                .speed(0.1),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("base colour");
                        let mut rgb = [
                            self.render_settings.render_base_color.x,
                            self.render_settings.render_base_color.y,
                            self.render_settings.render_base_color.z,
                        ];
                        if egui::color_picker::color_edit_button_rgb(ui, &mut rgb).changed() {
                            self.render_settings.render_base_color.x = rgb[0];
                            self.render_settings.render_base_color.y = rgb[1];
                            self.render_settings.render_base_color.z = rgb[2];
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("lerp colour");
                        let mut rgb = [
                            self.render_settings.render_lerp_color.x,
                            self.render_settings.render_lerp_color.y,
                            self.render_settings.render_lerp_color.z,
                        ];
                        if egui::color_picker::color_edit_button_rgb(ui, &mut rgb).changed() {
                            self.render_settings.render_lerp_color.x = rgb[0];
                            self.render_settings.render_lerp_color.y = rgb[1];
                            self.render_settings.render_lerp_color.z = rgb[2];
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("edge colour");
                        let mut rgb = [
                            self.render_settings.render_edge_color.x,
                            self.render_settings.render_edge_color.y,
                            self.render_settings.render_edge_color.z,
                        ];
                        if egui::color_picker::color_edit_button_rgb(ui, &mut rgb).changed() {
                            self.render_settings.render_edge_color.x = rgb[0];
                            self.render_settings.render_edge_color.y = rgb[1];
                            self.render_settings.render_edge_color.z = rgb[2];
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("edge distance");
                        ui.add(
                            egui::widgets::DragValue::new(&mut self.render_settings.edge_distance)
                                .range(0.0..=f32::MAX)
                                .speed(0.01),
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("saturation colour");
                        let mut rgb = [
                            self.render_settings.render_saturation_color.x,
                            self.render_settings.render_saturation_color.y,
                            self.render_settings.render_saturation_color.z,
                        ];
                        if egui::color_picker::color_edit_button_rgb(ui, &mut rgb).changed() {
                            self.render_settings.render_saturation_color.x = rgb[0];
                            self.render_settings.render_saturation_color.y = rgb[1];
                            self.render_settings.render_saturation_color.z = rgb[2];
                        }
                    });

                    ui.checkbox(&mut self.render_settings.show_force_field, "show force field");
                });


            egui::Window::new("objects")
                .resizable(true)
                .vscroll(true)
                .default_open(false)
                .show(self.egui.context(), |ui| { ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("threshold");
                        ui.add(
                            egui::widgets::DragValue::new(&mut store.threshold)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        );
                    });

                    ui.separator();

                    let mut remove_idx: Option<usize> = None;
                    for (i, quad) in store.quads.iter_mut().enumerate() {
                        ui.label(format!("object {i}"));

                        ui.horizontal(|ui| {
                            ui.label("position");
                            ui.add(egui::widgets::DragValue::new(&mut quad.pos.x).speed(0.1));
                            ui.add(egui::widgets::DragValue::new(&mut quad.pos.y).speed(0.1));
                            ui.add(egui::widgets::DragValue::new(&mut quad.pos.z).speed(0.1));
                        });

                        ui.horizontal(|ui| {
                            ui.label("scale");
                            ui.add(egui::widgets::DragValue::new(&mut quad.scale.x).speed(0.1));
                            ui.add(egui::widgets::DragValue::new(&mut quad.scale.y).speed(0.1));
                        });

                        ui.horizontal(|ui| {
                            ui.label("rotation");
                            ui.add(egui::widgets::DragValue::new(&mut quad.rot).speed(0.01));
                        });

                        let current = match quad.texture {
                            Texture::WHITE => "White",
                            Texture::NO_TEXTURE => "None",
                            Texture::CIRCLE => "Circle",
                            Texture::HCIRCLE => "Hollow",
                            other => "Image",
                        };
                        ComboBox::new(i, current)
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut quad.texture, Texture::WHITE, "White");
                                ui.selectable_value(&mut quad.texture, Texture::NO_TEXTURE, "None");
                                ui.selectable_value(&mut quad.texture, Texture::CIRCLE, "Circle");
                                ui.selectable_value(&mut quad.texture, Texture::HCIRCLE, "Hollow");
                            });

                        ui.horizontal(|ui| {
                            ui.label("colour");
                            let mut rgba = egui::Rgba::from_rgba_unmultiplied(
                                quad.colour.x, quad.colour.y, quad.colour.z, quad.colour.w,
                            );
                            if egui::color_picker::color_edit_button_rgba(ui, &mut rgba, egui::color_picker::Alpha::BlendOrAdditive).changed() {
                                quad.colour.x = rgba.r();
                                quad.colour.y = rgba.g();
                                quad.colour.z = rgba.b();
                                quad.colour.w = rgba.a();
                            }
                        });

                        if ui.button("remove").clicked() {
                            remove_idx = Some(i);
                        }

                        ui.separator();
                    }

                    if let Some(i) = remove_idx {
                        store.quads.remove(i);
                    }

                    ui.horizontal(|ui| {
                        if ui.button("Add").clicked() {
                            store.quads.push(Quad {
                                pos: Vec3::ZERO,
                                scale: Vec2::splat(1.0),
                                rot: 0.0,
                                colour: Vec4::ONE,
                                texture: Texture::CIRCLE,
                            });
                        }

                        if ui.button("Load image...").clicked() {
                            store.load_image_pending = true;
                        }
                    });

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

        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&"[molasses] render: post egui".into());


        self.staging_belt.finish();
        self.queue.submit(core::iter::once(encoder.finish()));
        self.staging_belt.recall();

        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&"[molasses] render: post submit".into());

        output.present();

        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&"[molasses] render: post present".into());


        // Queue a map of the staging buffer for the NEXT frame, but only if
        // we unmapped it this frame. On WASM the map may not have completed
        // yet, in which case we leave the existing map in flight.
        if !self.object_pipeline.buffer_mapped {
            *self.object_pipeline.map_complete.lock().unwrap() = false;
            let flag = std::sync::Arc::clone(&self.object_pipeline.map_complete);
            self.object_pipeline.staging_buffer.slice(..)
                .map_async(wgpu::MapMode::Read, move |_result| {
                    *flag.lock().unwrap() = true;
                });
            self.object_pipeline.buffer_mapped = true;
        }



        if restart_sim {
            self.restart_simulation();
        }
    }


    fn staging_buffer_ready(&self) -> bool {
        // Returns true when the map_async callback has fired and the
        // staging buffer is safe to read. The first frame after init has
        // no map in flight yet, so this returns false and the caller skips
        // the read; subsequent frames should see a completed map from the
        // previous frame's tail.
        *self.object_pipeline.map_complete.lock().unwrap()
    }


    pub fn restart_simulation(&mut self) {
        self.simulation = FluidSimulation::new(&self.device, self.sim_settings)
    }


    pub fn draw_rect(&mut self, store: &mut ObjectStore, pos: Vec2, rot: f32, extents: Vec2) {
        store.quads.push(Quad {
            pos: pos.extend(0.0),
            scale: extents,
            rot,
            colour: Vec4::ONE,
            texture: Texture::NO_TEXTURE,
        });
    }


    pub fn draw_circle(&mut self, store: &mut ObjectStore, pos: Vec2, radius: f32) {
        store.quads.push(Quad {
            pos: pos.extend(0.0),
            scale: Vec2::splat(radius * 2.0),
            rot: 0.0,
            colour: Vec4::ONE,
            texture: Texture::CIRCLE,
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


impl QuadInstance {
    fn desc() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: &[wgpu::VertexAttribute] = &[
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: core::mem::offset_of!(QuadInstance, colour) as _,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: core::mem::offset_of!(QuadInstance, uv) as _,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: core::mem::offset_of!(QuadInstance, pos) as _,
                shader_location: 3,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: core::mem::offset_of!(QuadInstance, scale) as _,
                shader_location: 4,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32,
                offset: core::mem::offset_of!(QuadInstance, rot) as _,
                shader_location: 5,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32,
                offset: core::mem::offset_of!(QuadInstance, z) as _,
                shader_location: 6,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: core::mem::offset_of!(QuadInstance, kind) as _,
                shader_location: 7,
            },
        ];

        wgpu::VertexBufferLayout {
            array_stride: size_of::<Self>() as _,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: ATTRS,
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



#[inline]
fn intersection(fa: f32, a: f32, fb: f32, b: f32) -> f32 {
    let den = 2.0 * (a - b);
    if den.abs() < 1e-12 { f32::INFINITY } else { (fa + a * a - fb - b * b) / den }
}



fn generate_smooth_gradient_field(img: Image) -> Vec<Vec2> {
    let height = img.height as usize;
    let width = img.width as usize;

    let mut dist = vec![vec![f32::MAX; width]; height];
    let mut has_white = false;

    for y in 0..height {
        for x in 0..width {
            if img.get_pixel(x as u32, y as u32) < 128 {
                dist[y][x] = 0.0;
                has_white = true;
            }
        }
    }

    if !has_white {
        for y in 0..height {
            for x in 0..width {
                if y == height - 1 || y == 0 || x == width - 1 || x == 0 {
                    dist[y][x] = 0.0;
                }
            }
        }
    }

    // Exact 2D Euclidean distance transform (Felzenszwalb & Huttenlocher, 2004).
    // Two passes of a 1D lower-envelope-of-parabolas, one per axis. Output is
    // a true Euclidean SDF (squared distance to nearest source pixel), exact
    // for every pixel -- no quantization, no chamfer approximation.
    //
    // 1D transform: for input f[i], compute d[x] = min_i (f[i] + (x-i)^2).
    // Maintains a lower envelope of parabolas in v[] with intersection points
    // in z[]. Standard linear-time algorithm.

    // First pass: along x, dist[y] -> dt[y] (still squared distances).
    let mut dt = vec![vec![0.0f32; width]; height];
    let mut v = vec![0usize; width];
    let mut z = vec![0.0f32; width + 1];

    for y in 0..height {
        let f = &dist[y];
        let mut k = 0usize;
        v[0] = 0;
        z[0] = f32::NEG_INFINITY;
        z[1] = f32::INFINITY;
        for q in 1..width {
            // Intersection s of parabola q with parabola v[k]:
            //   f[q] + (s-q)^2 = f[v[k]] + (s - v[k])^2
            //   s = (f[q] + q^2 - f[v[k]] - v[k]^2) / (2*(q - v[k]))
            let mut s = intersection(f[q], q as f32, f[v[k]], v[k] as f32);
            while k > 0 && s <= z[k] {
                k -= 1;
                s = intersection(f[q], q as f32, f[v[k]], v[k] as f32);
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
        }

        let mut k = 0usize;
        for x in 0..width {
            let xf = x as f32;
            while z[k + 1] < xf {
                k += 1;
            }
            let vk = v[k] as f32;
            dt[y][x] = f[v[k]] + (xf - vk) * (xf - vk);
        }
    }

    // Second pass: along y. After this, dist holds the true Euclidean distance.
    for x in 0..width {
        let mut k = 0usize;
        v[0] = 0;
        z[0] = f32::NEG_INFINITY;
        z[1] = f32::INFINITY;
        for q in 1..height {
            let mut s = intersection(dt[q][x], q as f32, dt[v[k]][x], v[k] as f32);
            while k > 0 && s <= z[k] {
                k -= 1;
                s = intersection(dt[q][x], q as f32, dt[v[k]][x], v[k] as f32);
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
        }

        let mut k = 0usize;
        for y in 0..height {
            let yf = y as f32;
            while z[k + 1] < yf {
                k += 1;
            }
            let vk = v[k] as f32;
            dist[y][x] = (dt[v[k]][x] + (yf - vk) * (yf - vk)).sqrt();
        }
    }

    let mut gradient_field = vec![Vec2::ZERO; width * height];
    if !has_white {
        return gradient_field;
    }

    for y in 0..height {
        for x in 0..width {
            let xm = if x > 0 { x - 1 } else { x };
            let xp = if x + 1 < width { x + 1 } else { x };
            let ym = if y > 0 { y - 1 } else { y };
            let yp = if y + 1 < height { y + 1 } else { y };

            let sdf = |xi: usize, yi: usize| -> f32 {
                dist[yi][xi]
            };

            let dx = sdf(xp, y) - sdf(xm, y);
            let dy = sdf(x, yp) - sdf(x, ym);
            let length = (dx * dx + dy * dy).sqrt();

            let grad_x = if length > 1e-6 { dx / length } else { 0.0 };
            let grad_y = if length > 1e-6 { dy / length } else { 0.0 };

            gradient_field[y * width + x] = -Vec2::new(grad_x, grad_y);
        }
    }


    gradient_field
}

