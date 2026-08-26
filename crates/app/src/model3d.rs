use std::{
    collections::HashMap,
    mem,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use asset_importer::{
    Importer,
    material::{Material, TextureInfo, TextureType},
    postprocess::PostProcessSteps,
    scene::Scene,
    texture::{Texture, TextureData},
};
use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::{project::Model3dShading, runtime::video::GpuFrame};

#[derive(Clone, Debug)]
struct ModelVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    color: [f32; 4],
}

#[derive(Clone, Debug)]
struct ModelTriangle {
    indices: [u32; 3],
    material: usize,
}

#[derive(Clone, Debug)]
struct ModelTexture {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ModelAlphaMode {
    #[default]
    Auto,
    Opaque,
    Mask,
    Blend,
}

#[derive(Clone, Debug)]
struct ModelMaterial {
    color: [f32; 4],
    texture: Option<ModelTexture>,
    opacity_texture: Option<ModelTexture>,
    normal_texture: Option<ModelTexture>,
    metallic_roughness_texture: Option<ModelTexture>,
    metallic_texture: Option<ModelTexture>,
    roughness_texture: Option<ModelTexture>,
    occlusion_texture: Option<ModelTexture>,
    emissive_texture: Option<ModelTexture>,
    transmission_texture: Option<ModelTexture>,
    uv_channel: usize,
    metallic: f32,
    roughness: f32,
    transmission: f32,
    normal_scale: f32,
    normal_is_height: bool,
    occlusion_strength: f32,
    emissive: [f32; 3],
    emissive_intensity: f32,
    alpha_mode: ModelAlphaMode,
    alpha_cutoff: f32,
}

impl Default for ModelMaterial {
    fn default() -> Self {
        Self {
            color: [0.76, 0.78, 0.82, 1.0],
            texture: None,
            opacity_texture: None,
            normal_texture: None,
            metallic_roughness_texture: None,
            metallic_texture: None,
            roughness_texture: None,
            occlusion_texture: None,
            emissive_texture: None,
            transmission_texture: None,
            uv_channel: 0,
            metallic: 0.0,
            roughness: 0.65,
            transmission: 0.0,
            normal_scale: 1.0,
            normal_is_height: false,
            occlusion_strength: 1.0,
            emissive: [0.0; 3],
            emissive_intensity: 1.0,
            alpha_mode: ModelAlphaMode::Auto,
            alpha_cutoff: 0.5,
        }
    }
}

impl ModelMaterial {
    fn is_transparent(&self) -> bool {
        if self.transmission > 0.001 || self.alpha_mode == ModelAlphaMode::Blend {
            return true;
        }
        if matches!(
            self.alpha_mode,
            ModelAlphaMode::Opaque | ModelAlphaMode::Mask
        ) {
            return false;
        }
        self.color[3] < 0.999
            || self.opacity_texture.as_ref().is_some_and(|texture| {
                texture
                    .pixels
                    .chunks_exact(4)
                    .any(|pixel| pixel[0] < 255 || pixel[3] < 255)
            })
            || self
                .texture
                .as_ref()
                .is_some_and(|texture| texture.pixels.chunks_exact(4).any(|pixel| pixel[3] < 255))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ModelMesh {
    vertices: Vec<ModelVertex>,
    triangles: Vec<ModelTriangle>,
    materials: Vec<ModelMaterial>,
    min: [f32; 3],
    max: [f32; 3],
}

impl ModelMesh {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let steps = PostProcessSteps::TRIANGULATE
            | PostProcessSteps::GEN_SMOOTH_NORMALS
            | PostProcessSteps::JOIN_IDENTICAL_VERTICES
            | PostProcessSteps::SORT_BY_PTYPE
            | PostProcessSteps::PRE_TRANSFORM_VERTICES;
        let scene = Importer::new()
            .read_file(path)
            .with_post_process(steps)
            .import()
            .with_context(|| format!("import 3D model {} with Assimp", path.display()))?;

        let mut materials = (0..scene.num_materials())
            .map(|index| {
                scene
                    .material(index)
                    .map(|material| load_material(&scene, path, &material))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        if materials.is_empty() {
            materials.push(ModelMaterial::default());
        }

        let mut vertices = Vec::new();
        let mut triangles = Vec::new();
        for mesh in scene.meshes() {
            let material = mesh.material_index().min(materials.len() - 1);
            let uv_channel = materials[material].uv_channel;
            let uvs = mesh.texture_coords2(uv_channel);
            let colors = mesh.vertex_colors(0);
            let normals = mesh.normals_raw_opt();
            let base = u32::try_from(vertices.len()).context("3D model has too many vertices")?;
            for (index, vertex) in mesh.vertices_raw().iter().enumerate() {
                let uv = uvs
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .map(|uv| [uv.x, uv.y])
                    .unwrap_or([0.0, 0.0]);
                let color = colors
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .map(|color| [color.x, color.y, color.z, color.w])
                    .unwrap_or([1.0, 1.0, 1.0, 1.0]);
                let normal = normals
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .map(|normal| [normal.x, normal.y, normal.z])
                    .unwrap_or([0.0, 0.0, 0.0]);
                vertices.push(ModelVertex {
                    position: [vertex.x, vertex.y, vertex.z],
                    normal,
                    uv,
                    color,
                });
            }
            for triangle in mesh.triangles_iter() {
                let a = base
                    .checked_add(triangle[0])
                    .context("3D model index overflow")?;
                let b = base
                    .checked_add(triangle[1])
                    .context("3D model index overflow")?;
                let c = base
                    .checked_add(triangle[2])
                    .context("3D model index overflow")?;
                triangles.push(ModelTriangle {
                    indices: [a, b, c],
                    material,
                });
            }
        }
        Self::from_parts(vertices, triangles, materials)
    }

    pub(crate) fn normalized_size(&self) -> [f32; 3] {
        let raw = [
            (self.max[0] - self.min[0]).abs(),
            (self.max[1] - self.min[1]).abs(),
            (self.max[2] - self.min[2]).abs(),
        ];
        let largest = raw.into_iter().fold(0.0_f32, f32::max).max(1.0e-6);
        raw.map(|component| {
            if component <= 1.0e-6 {
                0.01
            } else {
                component / largest * 2.0
            }
        })
    }

    fn from_parts(
        mut vertices: Vec<ModelVertex>,
        triangles: Vec<ModelTriangle>,
        materials: Vec<ModelMaterial>,
    ) -> Result<Self> {
        if vertices.is_empty() || triangles.is_empty() {
            bail!("3D model contains no renderable triangles");
        }
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for vertex in &vertices {
            for axis in 0..3 {
                if !vertex.position[axis].is_finite() {
                    bail!("3D model contains a non-finite vertex");
                }
                min[axis] = min[axis].min(vertex.position[axis]);
                max[axis] = max[axis].max(vertex.position[axis]);
            }
        }

        let mut accumulated = vec![[0.0_f32; 3]; vertices.len()];
        for triangle in &triangles {
            let [a, b, c] = triangle
                .indices
                .map(|index| vertices[index as usize].position);
            let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let face = [
                ab[1] * ac[2] - ab[2] * ac[1],
                ab[2] * ac[0] - ab[0] * ac[2],
                ab[0] * ac[1] - ab[1] * ac[0],
            ];
            for index in triangle.indices {
                for axis in 0..3 {
                    accumulated[index as usize][axis] += face[axis];
                }
            }
        }
        for (vertex, fallback) in vertices.iter_mut().zip(accumulated) {
            let length = (vertex.normal[0] * vertex.normal[0]
                + vertex.normal[1] * vertex.normal[1]
                + vertex.normal[2] * vertex.normal[2])
                .sqrt();
            if length <= 1.0e-6 {
                let fallback_length = (fallback[0] * fallback[0]
                    + fallback[1] * fallback[1]
                    + fallback[2] * fallback[2])
                    .sqrt()
                    .max(1.0e-6);
                vertex.normal = fallback.map(|value| value / fallback_length);
            } else {
                vertex.normal = vertex.normal.map(|value| value / length);
            }
        }

        Ok(Self {
            vertices,
            triangles,
            materials,
            min,
            max,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuModelVertex {
    position: [f32; 3],
    _position_pad: f32,
    normal: [f32; 3],
    _normal_pad: f32,
    uv: [f32; 2],
    _uv_pad: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SceneUniform {
    center: [f32; 4],
    extent: [f32; 4],
    size: [f32; 4],
    scale: [f32; 4],
    rotation: [f32; 4],
    position: [f32; 4],
    viewport: [f32; 4],
    shading: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MaterialUniform {
    base_color: [f32; 4],
    factors: [f32; 4],
    emissive: [f32; 4],
    texture_flags: [u32; 4],
    texture_flags2: [u32; 4],
    extra: [f32; 4],
}

struct GpuMaterial {
    bind_group: wgpu::BindGroup,
    transparent: bool,
    _uniform: wgpu::Buffer,
    _texture: wgpu::Texture,
    _opacity_texture: wgpu::Texture,
    _normal_texture: wgpu::Texture,
    _metallic_roughness_texture: wgpu::Texture,
    _metallic_texture: wgpu::Texture,
    _roughness_texture: wgpu::Texture,
    _occlusion_texture: wgpu::Texture,
    _emissive_texture: wgpu::Texture,
    _transmission_texture: wgpu::Texture,
}

struct GpuDraw {
    index_buffer: wgpu::Buffer,
    index_count: u32,
    material: usize,
}

struct GpuModel {
    vertex_buffer: wgpu::Buffer,
    draws: Vec<GpuDraw>,
    materials: Vec<GpuMaterial>,
    min: [f32; 3],
    max: [f32; 3],
}

fn upload_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    source: Option<&ModelTexture>,
    fallback: [u8; 4],
    srgb: bool,
) -> wgpu::Texture {
    let (width, height, pixels) = source
        .map(|texture| (texture.width, texture.height, texture.pixels.as_slice()))
        .unwrap_or((1, 1, fallback.as_slice()));
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: if srgb {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        },
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    texture
}

impl GpuModel {
    fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        material_layout: &wgpu::BindGroupLayout,
        mesh: ModelMesh,
    ) -> Result<Self> {
        let vertices = mesh
            .vertices
            .iter()
            .map(|vertex| GpuModelVertex {
                position: vertex.position,
                _position_pad: 0.0,
                normal: vertex.normal,
                _normal_pad: 0.0,
                uv: vertex.uv,
                _uv_pad: [0.0; 2],
                color: vertex.color,
            })
            .collect::<Vec<_>>();
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("kama 3D model vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let mut transparent_materials = mesh
            .materials
            .iter()
            .map(ModelMaterial::is_transparent)
            .collect::<Vec<_>>();
        for triangle in &mesh.triangles {
            let material = triangle.material.min(transparent_materials.len() - 1);
            if triangle
                .indices
                .iter()
                .any(|index| mesh.vertices[*index as usize].color[3] < 0.999)
            {
                transparent_materials[material] = true;
            }
        }

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kama 3D material sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let mut gpu_materials = Vec::with_capacity(mesh.materials.len());
        for (material_index, material) in mesh.materials.iter().enumerate() {
            let texture = upload_texture(
                device,
                queue,
                "kama 3D material base color",
                material.texture.as_ref(),
                [255, 255, 255, 255],
                true,
            );
            let opacity_texture = upload_texture(
                device,
                queue,
                "kama 3D material opacity",
                material.opacity_texture.as_ref(),
                [255, 255, 255, 255],
                false,
            );
            let normal_texture = upload_texture(
                device,
                queue,
                "kama 3D material normal",
                material.normal_texture.as_ref(),
                [128, 128, 255, 255],
                false,
            );
            let metallic_roughness_texture = upload_texture(
                device,
                queue,
                "kama 3D material metallic roughness",
                material.metallic_roughness_texture.as_ref(),
                [255, 255, 255, 255],
                false,
            );
            let metallic_texture = upload_texture(
                device,
                queue,
                "kama 3D material metallic",
                material.metallic_texture.as_ref(),
                [255, 255, 255, 255],
                false,
            );
            let roughness_texture = upload_texture(
                device,
                queue,
                "kama 3D material roughness",
                material.roughness_texture.as_ref(),
                [255, 255, 255, 255],
                false,
            );
            let occlusion_texture = upload_texture(
                device,
                queue,
                "kama 3D material occlusion",
                material.occlusion_texture.as_ref(),
                [255, 255, 255, 255],
                false,
            );
            let emissive_texture = upload_texture(
                device,
                queue,
                "kama 3D material emissive",
                material.emissive_texture.as_ref(),
                [0, 0, 0, 255],
                true,
            );
            let transmission_texture = upload_texture(
                device,
                queue,
                "kama 3D material transmission",
                material.transmission_texture.as_ref(),
                [255, 255, 255, 255],
                false,
            );

            let views = [
                texture.create_view(&wgpu::TextureViewDescriptor::default()),
                opacity_texture.create_view(&wgpu::TextureViewDescriptor::default()),
                normal_texture.create_view(&wgpu::TextureViewDescriptor::default()),
                metallic_roughness_texture.create_view(&wgpu::TextureViewDescriptor::default()),
                metallic_texture.create_view(&wgpu::TextureViewDescriptor::default()),
                roughness_texture.create_view(&wgpu::TextureViewDescriptor::default()),
                occlusion_texture.create_view(&wgpu::TextureViewDescriptor::default()),
                emissive_texture.create_view(&wgpu::TextureViewDescriptor::default()),
                transmission_texture.create_view(&wgpu::TextureViewDescriptor::default()),
            ];
            let uniform = MaterialUniform {
                base_color: material.color,
                factors: [
                    material.metallic,
                    material.roughness,
                    material.transmission,
                    material.normal_scale,
                ],
                emissive: [
                    material.emissive[0],
                    material.emissive[1],
                    material.emissive[2],
                    material.emissive_intensity,
                ],
                texture_flags: [
                    u32::from(material.normal_texture.is_some()),
                    u32::from(material.metallic_roughness_texture.is_some()),
                    u32::from(material.metallic_texture.is_some()),
                    u32::from(material.roughness_texture.is_some()),
                ],
                texture_flags2: [
                    u32::from(material.occlusion_texture.is_some()),
                    u32::from(material.emissive_texture.is_some()),
                    u32::from(material.transmission_texture.is_some()),
                    match material.alpha_mode {
                        ModelAlphaMode::Auto => 0,
                        ModelAlphaMode::Opaque => 1,
                        ModelAlphaMode::Mask => 2,
                        ModelAlphaMode::Blend => 3,
                    },
                ],
                extra: [
                    material.occlusion_strength,
                    material.alpha_cutoff,
                    u32::from(material.normal_is_height) as f32,
                    0.0,
                ],
            };
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("kama 3D material uniform"),
                contents: bytemuck::bytes_of(&uniform),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("kama 3D material bind group"),
                layout: material_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&views[0]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&views[1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&views[2]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&views[3]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(&views[4]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(&views[5]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 8,
                        resource: wgpu::BindingResource::TextureView(&views[6]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 9,
                        resource: wgpu::BindingResource::TextureView(&views[7]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 10,
                        resource: wgpu::BindingResource::TextureView(&views[8]),
                    },
                ],
            });
            gpu_materials.push(GpuMaterial {
                bind_group,
                transparent: transparent_materials[material_index],
                _uniform: uniform,
                _texture: texture,
                _opacity_texture: opacity_texture,
                _normal_texture: normal_texture,
                _metallic_roughness_texture: metallic_roughness_texture,
                _metallic_texture: metallic_texture,
                _roughness_texture: roughness_texture,
                _occlusion_texture: occlusion_texture,
                _emissive_texture: emissive_texture,
                _transmission_texture: transmission_texture,
            });
        }

        let mut grouped = vec![Vec::<u32>::new(); gpu_materials.len()];
        for triangle in &mesh.triangles {
            let material = triangle.material.min(grouped.len() - 1);
            grouped[material].extend_from_slice(&triangle.indices);
        }
        let mut draws = Vec::new();
        for (material, indices) in grouped.into_iter().enumerate() {
            if indices.is_empty() {
                continue;
            }
            let index_count =
                u32::try_from(indices.len()).context("3D model has too many indices")?;
            let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("kama 3D model indices"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            });
            draws.push(GpuDraw {
                index_buffer,
                index_count,
                material,
            });
        }

        Ok(Self {
            vertex_buffer,
            draws,
            materials: gpu_materials,
            min: mesh.min,
            max: mesh.max,
        })
    }
}

struct ModelRenderer {
    scene_layout: wgpu::BindGroupLayout,
    material_layout: wgpu::BindGroupLayout,
    opaque_pipeline: wgpu::RenderPipeline,
    transparent_pipeline: wgpu::RenderPipeline,
}

fn material_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn material_sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn material_layout_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    let mut entries = Vec::with_capacity(11);
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    });
    entries.push(material_texture_entry(1));
    entries.push(material_sampler_entry(2));
    entries.extend((3..=10).map(material_texture_entry));
    entries
}

impl ModelRenderer {
    fn new(device: &wgpu::Device) -> Self {
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kama 3D scene layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let material_entries = material_layout_entries();
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kama 3D material layout"),
            entries: &material_entries,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kama realtime 3D shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("model3d.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kama 3D pipeline layout"),
            bind_group_layouts: &[Some(&scene_layout), Some(&material_layout)],
            immediate_size: 0,
        });
        let premultiplied = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let make_pipeline =
            |label: &str, blend: Option<wgpu::BlendState>, depth_write_enabled: bool| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        buffers: &[Some(wgpu::VertexBufferLayout {
                            array_stride: mem::size_of::<GpuModelVertex>() as wgpu::BufferAddress,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &[
                                wgpu::VertexAttribute {
                                    format: wgpu::VertexFormat::Float32x3,
                                    offset: 0,
                                    shader_location: 0,
                                },
                                wgpu::VertexAttribute {
                                    format: wgpu::VertexFormat::Float32x3,
                                    offset: 16,
                                    shader_location: 1,
                                },
                                wgpu::VertexAttribute {
                                    format: wgpu::VertexFormat::Float32x2,
                                    offset: 32,
                                    shader_location: 2,
                                },
                                wgpu::VertexAttribute {
                                    format: wgpu::VertexFormat::Float32x4,
                                    offset: 48,
                                    shader_location: 3,
                                },
                            ],
                        })],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs_main"),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::Rgba16Float,
                            blend,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                    }),
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        strip_index_format: None,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode: None,
                        unclipped_depth: false,
                        polygon_mode: wgpu::PolygonMode::Fill,
                        conservative: false,
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: wgpu::TextureFormat::Depth32Float,
                        depth_write_enabled: Some(depth_write_enabled),
                        depth_compare: Some(wgpu::CompareFunction::Less),
                        stencil: wgpu::StencilState::default(),
                        bias: wgpu::DepthBiasState::default(),
                    }),
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                })
            };
        let opaque_pipeline = make_pipeline("kama realtime 3D opaque pipeline", None, true);
        let transparent_pipeline = make_pipeline(
            "kama realtime 3D transparent pipeline",
            Some(premultiplied),
            false,
        );
        Self {
            scene_layout,
            material_layout,
            opaque_pipeline,
            transparent_pipeline,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        model: &GpuModel,
        width: u32,
        height: u32,
        size: [f32; 3],
        scale: [f32; 3],
        rotation: [f32; 3],
        position: [f32; 3],
        shading: Model3dShading,
    ) -> GpuFrame {
        let width = width.max(1);
        let height = height.max(1);
        let center: [f32; 3] =
            std::array::from_fn(|axis| (model.min[axis] + model.max[axis]) * 0.5);
        let extent: [f32; 3] =
            std::array::from_fn(|axis| (model.max[axis] - model.min[axis]).abs().max(1.0e-6));
        let uniform = SceneUniform {
            center: [center[0], center[1], center[2], 0.0],
            extent: [extent[0], extent[1], extent[2], 0.0],
            size: [size[0], size[1], size[2], 0.0],
            scale: [scale[0], scale[1], scale[2], 0.0],
            rotation: [rotation[0], rotation[1], rotation[2], 0.0],
            position: [position[0], position[1], position[2], 0.0],
            viewport: [width as f32, height as f32, 0.0, 0.0],
            shading: [if shading == Model3dShading::Pbr { 1 } else { 0 }, 0, 0, 0],
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("kama 3D scene uniform"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let scene_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kama 3D scene bind group"),
            layout: &self.scene_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let output = GpuFrame::new(device, width, height, "kama realtime 3D render texture");
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kama realtime 3D depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kama realtime 3D pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output.view(),
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &scene_group, &[]);
            pass.set_vertex_buffer(0, model.vertex_buffer.slice(..));
            pass.set_pipeline(&self.opaque_pipeline);
            for draw in model
                .draws
                .iter()
                .filter(|draw| !model.materials[draw.material].transparent)
            {
                pass.set_bind_group(1, &model.materials[draw.material].bind_group, &[]);
                pass.set_index_buffer(draw.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..draw.index_count, 0, 0..1);
            }
            pass.set_pipeline(&self.transparent_pipeline);
            for draw in model
                .draws
                .iter()
                .filter(|draw| model.materials[draw.material].transparent)
            {
                pass.set_bind_group(1, &model.materials[draw.material].bind_group, &[]);
                pass.set_index_buffer(draw.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..draw.index_count, 0, 0..1);
            }
        }
        output
    }
}

pub(crate) struct ModelGpuRuntime {
    renderer: ModelRenderer,
    models: HashMap<PathBuf, Arc<GpuModel>>,
}

impl ModelGpuRuntime {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        Self {
            renderer: ModelRenderer::new(device),
            models: HashMap::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        path: &Path,
        width: u32,
        height: u32,
        size: [f32; 3],
        scale: [f32; 3],
        rotation: [f32; 3],
        position: [f32; 3],
        shading: Model3dShading,
    ) -> Result<GpuFrame> {
        let model = if let Some(model) = self.models.get(path) {
            Arc::clone(model)
        } else {
            let mesh = ModelMesh::load(path)?;
            let model = Arc::new(GpuModel::upload(
                device,
                queue,
                &self.renderer.material_layout,
                mesh,
            )?);
            self.models.insert(path.to_path_buf(), Arc::clone(&model));
            model
        };
        Ok(self.renderer.render(
            device, encoder, &model, width, height, size, scale, rotation, position, shading,
        ))
    }
}

pub(crate) fn probe_size(path: &Path) -> Result<[f32; 3]> {
    Ok(ModelMesh::load(path)?.normalized_size())
}

pub(crate) fn is_supported_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let extension = format!(".{extension}");
            asset_importer::is_extension_supported(&extension).unwrap_or(false)
        })
}

pub(crate) fn supported_extensions() -> Vec<String> {
    let mut extensions = asset_importer::get_import_extensions()
        .into_iter()
        .map(|extension| extension.trim_start_matches('.').to_ascii_lowercase())
        .filter(|extension| !extension.is_empty())
        .collect::<Vec<_>>();
    extensions.sort();
    extensions.dedup();
    extensions
}

fn load_material(scene: &Scene, model_path: &Path, material: &Material) -> ModelMaterial {
    let mut color = material
        .base_color()
        .map(|color| [color.x, color.y, color.z, color.w])
        .or_else(|| {
            material
                .diffuse_color()
                .map(|color| [color.x, color.y, color.z, 1.0])
        })
        .unwrap_or(ModelMaterial::default().color);
    if let Some(opacity) = material.opacity() {
        let opacity = opacity.clamp(0.0, 1.0);

        if (color[3] - opacity).abs() > 1.0e-5 {
            color[3] *= opacity;
        }
    } else if let Some(transparency) = material.transparency_factor() {
        color[3] *= 1.0 - transparency.clamp(0.0, 1.0);
    }

    let texture_info = material
        .base_color_texture(0)
        .or_else(|| material.texture(TextureType::Diffuse, 0));
    let opacity_info = material.texture(TextureType::Opacity, 0);
    let normal_info = material
        .texture(TextureType::Normals, 0)
        .or_else(|| material.texture(TextureType::NormalCamera, 0));

    let (normal_info, normal_is_height) = if let Some(normal) = normal_info {
        (Some(normal), false)
    } else {
        let height = material.texture(TextureType::Height, 0);
        let is_height = height.is_some();
        (height, is_height)
    };
    let metallic_roughness_info = material.texture(TextureType::GltfMetallicRoughness, 0);
    let metallic_info = metallic_roughness_info
        .is_none()
        .then(|| material.texture(TextureType::Metalness, 0))
        .flatten();
    let roughness_info = metallic_roughness_info
        .is_none()
        .then(|| material.texture(TextureType::DiffuseRoughness, 0))
        .flatten();
    let occlusion_info = material
        .texture(TextureType::AmbientOcclusion, 0)
        .or_else(|| material.texture(TextureType::Lightmap, 0));
    let emissive_info = material
        .texture(TextureType::EmissionColor, 0)
        .or_else(|| material.texture(TextureType::Emissive, 0));
    let transmission_info = material.texture(TextureType::Transmission, 0);
    let has_explicit_base_color =
        material.base_color().is_some() || material.diffuse_color().is_some();
    if !has_explicit_base_color && texture_info.is_some() {
        color[0] = 1.0;
        color[1] = 1.0;
        color[2] = 1.0;
    }
    let metallic_default = if metallic_roughness_info.is_some() || metallic_info.is_some() {
        1.0
    } else {
        0.0
    };
    let roughness_default = if metallic_roughness_info.is_some() || roughness_info.is_some() {
        1.0
    } else {
        0.65
    };

    let uv_channel = [
        texture_info.as_ref(),
        opacity_info.as_ref(),
        normal_info.as_ref(),
        metallic_roughness_info.as_ref(),
        metallic_info.as_ref(),
        roughness_info.as_ref(),
        occlusion_info.as_ref(),
        emissive_info.as_ref(),
        transmission_info.as_ref(),
    ]
    .into_iter()
    .flatten()
    .next()
    .map(|texture| texture.uv_index as usize)
    .unwrap_or(0);

    let load = |info: &Option<TextureInfo>| {
        info.as_ref()
            .and_then(|texture| load_material_texture(scene, model_path, texture))
    };
    let emissive = material
        .emissive_color()
        .map(|color| [color.x, color.y, color.z])
        .unwrap_or([0.0; 3]);

    let alpha_mode = material
        .get_string_property_str("$mat.gltf.alphaMode")
        .ok()
        .flatten()
        .map(|mode| match mode.to_ascii_uppercase().as_str() {
            "OPAQUE" => ModelAlphaMode::Opaque,
            "MASK" => ModelAlphaMode::Mask,
            "BLEND" => ModelAlphaMode::Blend,
            _ => ModelAlphaMode::Auto,
        })
        .unwrap_or(ModelAlphaMode::Auto);
    let alpha_cutoff = material
        .get_float_property_str("$mat.gltf.alphaCutoff")
        .ok()
        .flatten()
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);

    ModelMaterial {
        color,
        texture: load(&texture_info),
        opacity_texture: load(&opacity_info),
        normal_texture: load(&normal_info),
        metallic_roughness_texture: load(&metallic_roughness_info),
        metallic_texture: load(&metallic_info),
        roughness_texture: load(&roughness_info),
        occlusion_texture: load(&occlusion_info),
        emissive_texture: load(&emissive_info),
        transmission_texture: load(&transmission_info),
        uv_channel,
        metallic: material
            .metallic_factor()
            .unwrap_or(metallic_default)
            .clamp(0.0, 1.0),
        roughness: material
            .roughness_factor()
            .unwrap_or(roughness_default)
            .clamp(0.045, 1.0),
        transmission: material
            .transmission_factor()
            .unwrap_or(0.0)
            .clamp(0.0, 1.0),
        normal_scale: if normal_is_height {
            material.bump_scaling().unwrap_or(1.0)
        } else {
            material
                .normal_texture_scale(0)
                .or_else(|| material.bump_scaling())
                .unwrap_or(1.0)
        },
        normal_is_height,
        occlusion_strength: material
            .occlusion_texture_strength(0)
            .unwrap_or(1.0)
            .clamp(0.0, 1.0),
        emissive,
        emissive_intensity: material.emissive_intensity().unwrap_or(1.0).max(0.0),
        alpha_mode,
        alpha_cutoff,
    }
}

fn load_material_texture(
    scene: &Scene,
    model_path: &Path,
    info: &TextureInfo,
) -> Option<ModelTexture> {
    if let Ok(Some(texture)) = scene.embedded_texture_by_name(&info.path) {
        if let Some(texture) = decode_embedded_texture(&texture) {
            return Some(texture);
        }
    }
    if let Some(texture) = scene.find_texture_by_filename(&info.path) {
        if let Some(texture) = decode_embedded_texture(&texture) {
            return Some(texture);
        }
    }
    let normalized = info.path.replace('\\', "/");
    let source = Path::new(&normalized);
    let path = if source.is_absolute() {
        PathBuf::from(source)
    } else {
        model_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(source)
    };
    decode_image_file(&path)
}

fn decode_embedded_texture(texture: &Texture) -> Option<ModelTexture> {
    match texture.data().ok()? {
        TextureData::Texels(texels) => {
            let (width, height) = texture.dimensions();
            if width == 0 || height == 0 || texels.len() < width as usize * height as usize {
                return None;
            }
            let pixels = texels
                .into_iter()
                .take(width as usize * height as usize)
                .flat_map(|texel| [texel.r, texel.g, texel.b, texel.a])
                .collect();
            Some(ModelTexture {
                width,
                height,
                pixels,
            })
        }
        TextureData::Compressed(bytes) => decode_image_bytes(&bytes),
    }
}

fn decode_image_file(path: &Path) -> Option<ModelTexture> {
    let image = image::open(path).ok()?.to_rgba8();
    model_texture_from_rgba(image)
}

fn decode_image_bytes(bytes: &[u8]) -> Option<ModelTexture> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    model_texture_from_rgba(image)
}

fn model_texture_from_rgba(image: image::RgbaImage) -> Option<ModelTexture> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    Some(ModelTexture {
        width,
        height,
        pixels: image.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_layout_reserves_binding_two_for_the_shader_sampler() {
        let entries = material_layout_entries();
        assert_eq!(entries.len(), 11);
        assert!(matches!(entries[1].ty, wgpu::BindingType::Texture { .. }));
        assert!(matches!(
            entries[2].ty,
            wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)
        ));
        assert!(
            entries[3..]
                .iter()
                .all(|entry| matches!(entry.ty, wgpu::BindingType::Texture { .. }))
        );
    }
}
