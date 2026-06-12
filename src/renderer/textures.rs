use glam::{IVec2, Vec4};
use image::{GenericImage, RgbaImage};
use sti::{define_key, vec::KVec};
use wgpu::{BindGroup, BindGroupLayoutDescriptor, Device, Queue, Sampler, TextureFormat, TextureView};


define_key!(pub TextureAtlasId(u32));
define_key!(TextureId(u32));


#[derive(PartialEq, Clone, Copy, Debug)]
pub struct Texture(pub TextureAtlasId, TextureId);


#[derive(Debug)]
pub struct AtlasManager {
    atlases: KVec<TextureAtlasId, TextureAtlas>,
    pub bgl: wgpu::BindGroupLayout,
}


#[derive(Debug)]
pub struct TextureAtlas {
    _id: TextureAtlasId,

    uvs: KVec<TextureId, Vec4>,

    _view: TextureView,
    _sampler: Sampler,
    bind_group: BindGroup,
}


pub struct TextureAtlasBuilder<'a> {
    atlas_manager: &'a mut AtlasManager,

    max_dims: IVec2,
    id: TextureAtlasId,

    textures: KVec<TextureId, RgbaImage>,
    data_format: TextureFormat,
}


impl Texture {
    pub const WHITE : Self = Self(TextureAtlasId(0), TextureId(0));
    pub const NO_TEXTURE : Self = Self(TextureAtlasId(0), TextureId(1));
    pub const CIRCLE: Self = Self(TextureAtlasId(1), TextureId(0));
    pub const HCIRCLE: Self = Self(TextureAtlasId(1), TextureId(1));
}


impl AtlasManager {
    pub fn new(device: &Device, queue: &Queue) -> Self {

        let bgl = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("ui-texture-atlas-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });


        let mut this = Self {
            atlases: KVec::new(),
            bgl,
        };

        let mut atlas = this.create_atlas(TextureFormat::Rgba8UnormSrgb);
        atlas.register(IVec2::new(4, 4), &[255; 64]);
        atlas.register(IVec2::new(2, 2), &[
            227, 49, 239, 255,
            0, 0, 0, 255,
            0, 0, 0, 255,
            227, 49, 239, 255,
        ]);
        atlas.finalize(device, queue);

        let mut atlas = this.create_atlas(TextureFormat::Rgba8UnormSrgb);

        let circle_radius = 512i32;
        let mut circle_data = vec![0; (circle_radius * circle_radius * 4 * 4) as usize];
        let mut hcircle_data = vec![0; (circle_radius * circle_radius * 4 * 4) as usize];

        for y in 0..circle_radius * 2 {
            for x in 0..circle_radius * 2 {
                let dx = circle_radius - x;
                let dy = circle_radius - y;
                let dist = dx*dx + dy*dy;
                if dist > circle_radius*circle_radius { continue; }

                let idx = y * 2 * circle_radius * 4 + x * 4;
                let idx = idx as usize;

                circle_data[idx] = 255;
                circle_data[idx+1] = 255;
                circle_data[idx+2] = 255;
                circle_data[idx+3] = 255;

                if dist > circle_radius*circle_radius / 4 {
                    hcircle_data[idx] = 255;
                    hcircle_data[idx+1] = 255;
                    hcircle_data[idx+2] = 255;
                    hcircle_data[idx+3] = 255;
                }
            }
        }

        atlas.register(IVec2::splat(circle_radius * 2), &circle_data);
        atlas.register(IVec2::splat(circle_radius * 2), &hcircle_data);

        atlas.finalize(device, queue);

        this
    }


    pub fn create_atlas<'a>(&'a mut self, format: TextureFormat) -> TextureAtlasBuilder<'a> {
        TextureAtlasBuilder {
            max_dims: IVec2::ZERO, 
            id: self.atlases.klen(), 
            textures: KVec::new(), 
            data_format: format,
            atlas_manager: self, 
        }
    }


    pub fn get_uv(&self, texture: Texture) -> Vec4 {
        self.atlases[texture.0].uvs[texture.1]
    }


    pub fn get_bg(&self, atlas_id: TextureAtlasId) -> &BindGroup {
        &self.atlases[atlas_id].bind_group
    }
}


impl TextureAtlasBuilder<'_> {
    pub fn register(&mut self, dim: IVec2, data: &[u8]) -> Texture {
        assert!(
            matches!(
                self.data_format,
                TextureFormat::Rgba8Unorm | TextureFormat::Rgba8UnormSrgb
            ),
            "image-based atlas requires RGBA8 data, got {:?}",
            self.data_format
        );
        let pixel_size = self.data_format.block_copy_size(Some(wgpu::TextureAspect::All)).unwrap();
        assert_eq!(dim.x * dim.y * pixel_size as i32, data.len() as i32,
                   "format: {:?}, pixel_size: {pixel_size}, dims: {dim}", self.data_format);
        self.max_dims = self.max_dims.max(dim);
        let image = RgbaImage::from_raw(dim.x as u32, dim.y as u32, data.to_vec())
            .expect("invalid image dimensions for atlas registration");

        Texture(self.id, self.textures.push(image))
    }

    pub fn finalize(self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.textures.is_empty() {
            return;
        }

        let max_side = device.limits().max_texture_dimension_2d;

        let pixel_size = self
            .data_format
            .block_copy_size(Some(wgpu::TextureAspect::All))
            .expect("format must be copyable");

        // Padding / extrusion
        let p: u32 = 2;

        let cell_w: u32 = self.max_dims.x as u32 + 2 * p;
        let cell_h: u32 = self.max_dims.y as u32 + 2 * p;

        // Sanity: all sprites must fit inside the padded cell
        for texture in self.textures.iter() {
            let (w, h) = texture.dimensions();
            assert!(
                w + 2 * p <= cell_w && h + 2 * p <= cell_h,
                "sprite {:?} ({}x{}) doesn't fit in cell ({}x{}) with padding {}",
                texture.as_raw().as_ptr(), w, h, cell_w, cell_h, p
            );
        }

        // Choose smallest power-of-two side that can hold N cells (capacity, not area!)
        let n = self.textures.len() as u32;

        let mut line: u32 = 0;
        let mut cols: u32 = 0;
        let mut rows: u32 = 0;

        let max_pow = max_side.ilog2();
        for pow in 0..=max_pow {
            let side = 1u32 << pow;

            let c = side / cell_w;
            let r = side / cell_h;
            let cap = c.saturating_mul(r);

            if cap >= n && c > 0 && r > 0 {
                line = side;
                cols = c;
                rows = r;
                break;
            }
        }

        assert!(line != 0, "could not fit atlas within max texture side {max_side}");
        assert!(line <= max_side, "chosen atlas side exceeds device limit");

        // Allocate atlas buffer
        let mut atlas_image = RgbaImage::new(line, line);
        let mut uvs = KVec::from_iter((0..self.textures.len()).map(|_| Vec4::ZERO));

        let pixel_uv = 1.0 / line as f32;


        for (slot, texture) in self.textures.iter().enumerate() {
            let slot = slot as u32;
            let row = slot / cols;
            let col = slot % cols;

            debug_assert!(row < rows);

            let (w, h) = texture.dimensions();

            let cell_x = col * cell_w;
            let cell_y = row * cell_h;

            let dst_x0 = cell_x + p;
            let dst_y0 = cell_y + p;

            atlas_image.copy_from(texture, dst_x0, dst_y0).unwrap();


            // padding
                            
            // After atlas_image.copy_from(texture, dst_x0, dst_y0)?;
            let x0 = dst_x0;
            let y0 = dst_y0;
            let x1 = dst_x0 + w - 1;
            let y1 = dst_y0 + h - 1;

            // Left/right padding for each row. We copy the edge pixel for RGB
            // (so linear filtering gives a clean border) but force alpha=0 so
            // the new shader's `lum * alpha` writes 0 outside the texture's
            // actual content, not the bounding rectangle.
            for y in 0..h {
                let sy = y0 + y;
                let left = *atlas_image.get_pixel(x0, sy);
                let right = *atlas_image.get_pixel(x1, sy);
                for px in 1..=p {
                    let mut left_pad = left;
                    left_pad.0[3] = 0;
                    atlas_image.put_pixel(x0 - px, sy, left_pad);
                    let mut right_pad = right;
                    right_pad.0[3] = 0;
                    atlas_image.put_pixel(x1 + px, sy, right_pad);
                }
            }

            // Top/bottom padding for each column (including the padded width).
            for x in (x0 - p)..=(x1 + p) {
                let top = *atlas_image.get_pixel(x, y0);
                let bot = *atlas_image.get_pixel(x, y1);
                for py in 1..=p {
                    let mut top_pad = top;
                    top_pad.0[3] = 0;
                    atlas_image.put_pixel(x, y0 - py, top_pad);
                    let mut bot_pad = bot;
                    bot_pad.0[3] = 0;
                    atlas_image.put_pixel(x, y1 + py, bot_pad);
                }
            }

            // handle uvs
            let x0 = dst_x0;
            let y0 = dst_y0;
            let x1 = dst_x0 + w;
            let y1 = dst_y0 + h;

            let u0 = x0 as f32 * pixel_uv;
            let v0 = y0 as f32 * pixel_uv;
            let u1 = x1 as f32 * pixel_uv;
            let v1 = y1 as f32 * pixel_uv;

            let uv = Vec4::new(
                u0, v1,
                u1, v0,
            );

            uvs[TextureId(slot as _)] = uv;
        }

        let buffer = atlas_image.as_raw();

        // Create GPU texture
        let texture_size = wgpu::Extent3d {
            width: line,
            height: line,
            depth_or_array_layers: 1,
        };

        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("texture-atlas-texture"),
            size: texture_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.data_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("texture-atlas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &atlas_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            buffer,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pixel_size * line),
                rows_per_image: Some(line),
            },
            texture_size,
        );

        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui-texture-bind-group"),
            layout: &self.atlas_manager.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let atlas = TextureAtlas {
            uvs,
            _view: view,
            _sampler: sampler,
            _id: self.id,
            bind_group: bg,
        };

        if self.atlas_manager.atlases.len() > self.id.0 as usize {
            self.atlas_manager.atlases[self.id] = atlas;
        } else {
            assert_eq!(self.atlas_manager.atlases.push(atlas), self.id);
        }
    }


}


impl Default for Texture {
    fn default() -> Self {
        Self::WHITE
    }
}
