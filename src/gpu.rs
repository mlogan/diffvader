//! wgpu setup and the single instanced-quad pipeline.
//!
//! Everything on screen (backgrounds, glyphs, status bar) is a `Quad` instance that samples
//! the glyph atlas; solid rectangles sample a white block. A frame is one vertex buffer
//! upload and one draw call per scissor region.

use std::sync::Arc;

use winit::window::Window;

use crate::font::Atlas;
use crate::trace;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Quad {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub uv: [f32; 4],
    pub color: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Batch {
    /// Scissor rect in physical pixels (x, y, w, h).
    pub scissor: [u32; 4],
    pub start: u32,
    pub end: u32,
}

#[derive(Default)]
pub struct DrawList {
    pub quads: Vec<Quad>,
    pub batches: Vec<Batch>,
}

impl DrawList {
    pub fn clear(&mut self) {
        self.quads.clear();
        self.batches.clear();
    }

    /// Starts a new scissor region; subsequent quads belong to it.
    pub fn begin(&mut self, scissor: [u32; 4]) {
        self.end_batch();
        let start = self.quads.len() as u32;
        self.batches.push(Batch {
            scissor,
            start,
            end: start,
        });
    }

    /// Closes the last batch; must be called once building is complete.
    pub fn finish(&mut self) {
        self.end_batch();
    }

    fn end_batch(&mut self) {
        if let Some(b) = self.batches.last_mut() {
            b.end = self.quads.len() as u32;
        }
    }

    #[inline]
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: u32, white_uv: [f32; 4]) {
        self.quads.push(Quad {
            pos: [x, y],
            size: [w, h],
            uv: white_uv,
            color,
        });
    }
}

#[derive(Clone, Copy, Debug)]
pub enum SurfaceProblem {
    /// Skip this frame and try again.
    Timeout,
    /// The window is not visible yet (or is hidden); nothing was presented.
    Occluded,
    /// The surface must be reconfigured (resize/lost) before the next frame.
    Reconfigure,
    Fatal,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    screen: [f32; 2],
    _pad: [f32; 2],
}

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    vbuf: wgpu::Buffer,
    vbuf_cap: usize,
    atlas_tex: wgpu::Texture,
    atlas_size: (u32, u32),
    offscreen: Option<wgpu::Texture>,
}

const SHADER: &str = r#"
struct Uniforms { screen: vec2<f32>, _pad: vec2<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct In {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv: vec4<f32>,
    @location(3) color: u32,
};
struct Out {
    @builtin(position) p: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, i: In) -> Out {
    // Two triangles: (0,0) (1,0) (0,1) / (0,1) (1,0) (1,1)
    let cx = f32(vi == 1u || vi == 4u || vi == 5u);
    let cy = f32(vi == 2u || vi == 3u || vi == 5u);
    let corner = vec2<f32>(cx, cy);
    let px = i.pos + corner * i.size;
    let ndc = vec2<f32>(px.x / u.screen.x * 2.0 - 1.0, 1.0 - px.y / u.screen.y * 2.0);
    var o: Out;
    o.p = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = mix(i.uv.xy, i.uv.zw, corner);
    o.color = unpack4x8unorm(i.color);
    return o;
}

@fragment
fn fs(i: Out) -> @location(0) vec4<f32> {
    let a = textureSample(atlas, samp, i.uv).r;
    return vec4<f32>(i.color.rgb, i.color.a * a);
}
"#;

/// Instance, adapter and device: everything that does not need a window. Created on a
/// background thread at process start so it overlaps AppKit initialization.
pub struct GpuCore {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl GpuCore {
    pub fn init() -> Result<GpuCore, String> {
        let _s = trace::span("gpu-core-init");
        let instance = {
            let _s = trace::span("gpu-instance");
            wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::METAL,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            })
        };
        let adapter = {
            let _s = trace::span("gpu-adapter");
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                ..Default::default()
            }))
            .map_err(|e| format!("request_adapter: {e}"))?
        };
        let (device, queue) = {
            let _s = trace::span("gpu-device");
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("diffvader"),
                ..Default::default()
            }))
            .map_err(|e| format!("request_device: {e}"))?
        };
        Ok(GpuCore {
            instance,
            adapter,
            device,
            queue,
        })
    }
}

impl Gpu {
    pub fn new(core: GpuCore, window: Arc<Window>, atlas: &Atlas) -> Result<Gpu, String> {
        let _s = trace::span("gpu-init");
        let GpuCore {
            instance,
            adapter,
            device,
            queue,
        } = core;
        let size = window.inner_size();
        let surface = {
            let _s = trace::span("gpu-surface");
            instance
                .create_surface(window)
                .map_err(|e| format!("create_surface: {e}"))?
        };

        let caps = surface.get_capabilities(&adapter);
        // A non-sRGB format: theme colors are specified in sRGB and written through unchanged.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("surface not supported by adapter")?;
        config.format = format;
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        {
            let _s = trace::span("gpu-configure");
            surface.configure(&device, &config);
        }

        let shader = {
            let _s = trace::span("gpu-shader");
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("quad"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            })
        };
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quad"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = {
            let _s = trace::span("gpu-pipeline");
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("quad"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Quad>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Uint32],
                    })],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::SrcAlpha,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let atlas_tex = create_atlas_texture(&device, atlas.width, atlas.height);
        let bind_group = create_bind_group(&device, &bind_layout, &uniforms, &atlas_tex, &sampler);
        let vbuf_cap = 1 << 16;
        let vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quads"),
            size: (vbuf_cap * std::mem::size_of::<Quad>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut gpu = Gpu {
            device,
            queue,
            surface,
            config,
            pipeline,
            bind_layout,
            bind_group,
            sampler,
            uniforms,
            vbuf,
            vbuf_cap,
            atlas_tex,
            atlas_size: (atlas.width, atlas.height),
            offscreen: None,
        };
        gpu.write_uniforms();
        Ok(gpu)
    }

    fn write_uniforms(&mut self) {
        let u = Uniforms {
            screen: [self.config.width as f32, self.config.height as f32],
            _pad: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&u));
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let _s = trace::span("gpu-resize");
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.write_uniforms();
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Uploads the atlas region that changed since the last call.
    pub fn sync_atlas(&mut self, atlas: &mut Atlas) {
        if (atlas.width, atlas.height) != self.atlas_size {
            self.atlas_tex = create_atlas_texture(&self.device, atlas.width, atlas.height);
            self.atlas_size = (atlas.width, atlas.height);
            self.bind_group = create_bind_group(
                &self.device,
                &self.bind_layout,
                &self.uniforms,
                &self.atlas_tex,
                &self.sampler,
            );
            atlas.dirty = [0, 0, atlas.width, atlas.height];
        }
        let [x0, y0, x1, y1] = atlas.dirty;
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let _s = trace::span_arg("atlas-upload", ((x1 - x0) * (y1 - y0)) as u64);
        // Rows are uploaded at full atlas width so the source layout matches the CPU copy;
        // only the changed row band is sent.
        let offset = (y0 * atlas.width) as usize;
        let end = (y1 * atlas.width) as usize;
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.atlas_tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: y0, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &atlas.pixels[offset..end],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas.width),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: atlas.width,
                height: y1 - y0,
                depth_or_array_layers: 1,
            },
        );
        atlas.clear_dirty();
    }

    /// Uploads the draw list and presents one frame. Returns `Err` if the surface was lost
    /// or is outdated, in which case the caller should reconfigure and retry.
    pub fn render(&mut self, list: &DrawList, clear: [f64; 4]) -> Result<(), SurfaceProblem> {
        let _s = trace::span_arg("gpu-render", list.quads.len() as u64);
        self.upload_quads(list);
        let frame = {
            let _s = trace::span("gpu-acquire");
            match self.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(t)
                | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                wgpu::CurrentSurfaceTexture::Timeout => {
                    trace::mark("surface-timeout");
                    return Err(SurfaceProblem::Timeout);
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    trace::mark("surface-outdated");
                    return Err(SurfaceProblem::Reconfigure);
                }
                wgpu::CurrentSurfaceTexture::Occluded => {
                    trace::mark("surface-occluded");
                    return Err(SurfaceProblem::Occluded);
                }
                wgpu::CurrentSurfaceTexture::Validation => return Err(SurfaceProblem::Fatal),
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let encoder = self.encode_pass(list, clear, &view);
        {
            let _s = trace::span("gpu-submit");
            self.queue.submit(Some(encoder));
        }
        {
            let _s = trace::span("gpu-present");
            self.queue.present(frame);
        }
        Ok(())
    }

    fn encode_pass(
        &self,
        list: &DrawList,
        clear: [f64; 4],
        view: &wgpu::TextureView,
    ) -> wgpu::CommandBuffer {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear[0],
                            g: clear[1],
                            b: clear[2],
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vbuf.slice(..));
            let (sw, sh) = (self.config.width, self.config.height);
            for b in &list.batches {
                if b.end <= b.start {
                    continue;
                }
                let [x, y, w, h] = b.scissor;
                let x = x.min(sw);
                let y = y.min(sh);
                let w = w.min(sw - x);
                let h = h.min(sh - y);
                if w == 0 || h == 0 {
                    continue;
                }
                pass.set_scissor_rect(x, y, w, h);
                pass.draw(0..6, b.start..b.end);
            }
        }
        encoder.finish()
    }

    /// Renders the draw list to an offscreen texture and blocks until the GPU has finished.
    /// Used by `--bench-scroll` so frame cost can be measured without a visible window.
    pub fn render_offscreen(&mut self, list: &DrawList, clear: [f64; 4]) {
        let _s = trace::span_arg("gpu-render-offscreen", list.quads.len() as u64);
        let (w, h) = (self.config.width, self.config.height);
        if self.offscreen.as_ref().map(|t| (t.width(), t.height())) != Some((w, h)) {
            self.offscreen = Some(self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("offscreen"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            }));
        }
        self.upload_quads(list);
        let view = self
            .offscreen
            .as_ref()
            .unwrap()
            .create_view(&wgpu::TextureViewDescriptor::default());
        let pass = self.encode_pass(list, clear, &view);
        self.queue.submit([pass]);
        let _w = trace::span("gpu-wait");
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }

    fn upload_quads(&mut self, list: &DrawList) {
        if list.quads.len() > self.vbuf_cap {
            let mut cap = self.vbuf_cap;
            while cap < list.quads.len() {
                cap *= 2;
            }
            self.vbuf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("quads"),
                size: (cap * std::mem::size_of::<Quad>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.vbuf_cap = cap;
        }
        let _s = trace::span("gpu-upload-quads");
        if !list.quads.is_empty() {
            self.queue
                .write_buffer(&self.vbuf, 0, bytemuck::cast_slice(&list.quads));
        }
    }

    /// Renders the draw list to an offscreen texture and returns tightly packed RGBA8 pixels.
    pub fn render_to_image(&mut self, list: &DrawList, clear: [f64; 4]) -> (u32, u32, Vec<u8>) {
        let (w, h) = (self.config.width, self.config.height);
        self.upload_quads(list);
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screenshot"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let pass = self.encode_pass(list, clear, &view);
        let bpr = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot"),
            size: (bpr * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([pass, enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buf.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv()
            .expect("map callback")
            .expect("map screenshot buffer");
        let data = buf.get_mapped_range(..).expect("mapped range");
        let bgra = matches!(
            self.config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * bpr) as usize;
            let line = &data[start..start + (w * 4) as usize];
            if bgra {
                for px in line.chunks_exact(4) {
                    out.extend_from_slice(&[px[2], px[1], px[0], 255]);
                }
            } else {
                out.extend_from_slice(line);
            }
        }
        drop(data);
        buf.unmap();
        (w, h, out)
    }
}

fn create_atlas_texture(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atlas"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    atlas: &wgpu::Texture,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("quad"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}
