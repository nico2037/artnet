//! Deferred renderer: one instanced cone per beam (light volume), so cost scales with beam screen area.

use std::sync::Arc;

use anyhow::Result;
use glam::{Mat3, Mat4, Vec3};
use glow::HasContext;

use crate::fixtures::{build_beam_instances, build_body_instances, build_light_instances, Fixture, ModelInstance, BODY_FLOATS};
use crate::gl_util::{self as gu, Layout, U};
use crate::meshes::{cone_mesh, fixture_body_mesh};
use crate::resources::{self, GOBO_SIZE};

const CONE_SEGMENTS: usize = 24;
const HAZE_DENSITY: f32 = 0.1;
const SURFACE_GAIN: f32 = 50.0;
const VOLUME_GAIN: f32 = 60.0;
const AMBIENT: f32 = 0.006;
const EXTINCTION: f32 = 0.02;
const HG_G: f32 = 0.35;
const ATLAS_MAX: i32 = 8192;
const MIN_VOLUME_SCALE: f32 = 0.2;

#[derive(Clone, Copy, Debug)]
pub struct Quality {
    pub volume_scale: f32,
    pub steps: i32,
    pub noise: f32,
    pub shadow_tile: i32,
    pub light_tile: i32,
}

pub fn quality(name: &str) -> Option<Quality> {
    let q = |volume_scale, steps, noise, shadow_tile, light_tile| Quality { volume_scale, steps, noise, shadow_tile, light_tile };
    match name {
        "low" => Some(q(0.33, 12, 0.0, 128, 256)),
        "medium" => Some(q(0.5, 24, 1.0, 128, 256)),
        "high" => Some(q(0.75, 48, 1.0, 256, 512)),
        _ => None,
    }
}

/// Halves the tile size until all tiles fit; returns (tile, tiles per row, atlas size).
fn atlas_layout(tiles: i32, mut tile: i32, max_size: i32) -> (i32, i32, (i32, i32)) {
    let max_size = max_size.min(ATLAS_MAX);
    while tile > 16 && tiles > (max_size / tile).pow(2) {
        tile /= 2;
    }
    let per_row = tiles.min(max_size / tile);
    (tile, per_row, (per_row * tile, (tiles + per_row - 1) / per_row * tile))
}

/// Cube faces in the order the shader selects them: +x -x +y -y +z -z.
fn face_rotations() -> [Mat3; 6] {
    let faces = [
        (Vec3::X, Vec3::NEG_Y), (Vec3::NEG_X, Vec3::NEG_Y), (Vec3::Y, Vec3::Z),
        (Vec3::NEG_Y, Vec3::NEG_Z), (Vec3::Z, Vec3::NEG_Y), (Vec3::NEG_Z, Vec3::NEG_Y),
    ];
    faces.map(|(dir, up)| Mat3::from_mat4(Mat4::look_at_rh(Vec3::ZERO, dir, up)))
}

fn scaled(size: (i32, i32), s: f32) -> (i32, i32) {
    (((size.0 as f32 * s) as i32).max(1), ((size.1 as f32 * s) as i32).max(1))
}

fn v2(v: (i32, i32)) -> U<'static> {
    U::V2([v.0 as f32, v.1 as f32])
}

struct Atlas {
    tex: glow::Texture,
    fbo: glow::Framebuffer,
    size: (i32, i32),
    grid: [f32; 2], // tiles per row, tile size
}

pub struct Renderer {
    pub gl: Arc<glow::Context>,
    pub size: (i32, i32),
    pub quality: Quality,
    pub volume_scale: f32,
    pub gobo_count: usize,
    gobos: glow::Texture,
    noise: glow::Texture,

    scene_prog: glow::Program,
    body_prog: glow::Program,
    surface_prog: glow::Program,
    volume_prog: glow::Program,
    composite_prog: glow::Program,
    shadow_prog: glow::Program,
    light_image_prog: glow::Program,

    scene_vao: glow::VertexArray,
    scene_vbo: glow::Buffer,
    scene_vertices: i32,
    shadow_vao: glow::VertexArray,
    body_vao: glow::VertexArray,
    body_vertices: i32,
    body_inst: glow::Buffer,
    model_prog: glow::Program,
    model_vbo: glow::Buffer,
    model_inst: glow::Buffer,
    model_vao: glow::VertexArray,
    /// First vertex and vertex count per mesh id.
    model_ranges: Vec<(i32, i32)>,
    /// Instance data grouped by mesh: (mesh, floats).
    model_groups: Vec<(u32, Vec<f32>)>,
    surface_vao: glow::VertexArray,
    volume_vao: glow::VertexArray,
    cone_vertices: i32,
    mesh_scale: f32,
    beam_inst: glow::Buffer,
    light_image_vao: glow::VertexArray,
    light_image_inst: glow::Buffer,
    composite_vao: glow::VertexArray,

    albedo: glow::Texture,
    normal: glow::Texture,
    light: glow::Texture,
    depth: glow::Texture,
    gbuffer: glow::Framebuffer,
    light_fbo: glow::Framebuffer,
    volume: glow::Texture,
    volume_tex_size: (i32, i32),
    volume_fbo: glow::Framebuffer,

    face_rot: [Mat3; 6],
    shadow: Option<Atlas>,
    shadow_key: Vec<Vec3>,
    light_image: Option<Atlas>,
    light_image_count: usize,
}

impl Renderer {
    /// `gobos`: GOBO_SIZE² R8 layers. The demo's wheels use the whole library; GDTF fixtures pass
    /// their wheel's layer range per fixture (`Fixture::gobo_range`).
    pub fn new(gl: Arc<glow::Context>, size: (i32, i32), quality: Quality, gobos: &[Vec<u8>], scene_vertices: &[f32]) -> Result<Self> {
        unsafe {
            let gobo_count = gobos.len();
            let gobos = resources::upload_gobos(&gl, gobos)?;
            let noise = resources::noise_texture(&gl)?;

            let prog = |vs: &str, fs: &str, defines: &[&str], common: bool| gu::program(&gl, vs, fs, defines, common);
            let scene_prog = prog("scene.vert", "scene.frag", &[], false)?;
            let body_prog = prog("fixture.vert", "scene.frag", &[], false)?;
            let surface_prog = prog("beam.vert", "beam.frag", &["SURFACE", "COLOR_IMAGE"], true)?;
            let volume_prog = prog("beam.vert", "beam.frag", &["VOLUME", "COLOR_IMAGE"], true)?;
            let composite_prog = prog("fullscreen.vert", "composite.frag", &[], false)?;
            let shadow_prog = prog("shadow.vert", "shadow.frag", &[], false)?;
            let light_image_prog = prog("lightimage.vert", "lightimage.frag", &["COLOR_IMAGE"], false)?;

            let scene_vbo = gu::buffer(&gl, scene_vertices, glow::STATIC_DRAW)?;
            let scene_vao = gu::vertex_array(&gl, scene_prog, &[Layout {
                buffer: scene_vbo, attrs: &[("in_pos", 3), ("in_normal", 3), ("in_albedo", 3)], instanced: false,
            }])?;
            let shadow_vao = gu::vertex_array(&gl, shadow_prog, &[Layout {
                buffer: scene_vbo, attrs: &[("in_pos", 3), ("", 6)], instanced: false,
            }])?;

            let body = fixture_body_mesh();
            let body_vbo = gu::buffer(&gl, &body, glow::STATIC_DRAW)?;
            let body_inst = gu::buffer(&gl, &[], glow::DYNAMIC_DRAW)?;
            let body_vao = gu::vertex_array(&gl, body_prog, &[
                Layout { buffer: body_vbo, attrs: &[("in_pos", 3), ("in_normal", 3), ("in_part", 1)], instanced: false },
                Layout {
                    buffer: body_inst,
                    attrs: &[
                        ("i_pos", 3), ("i_m0", 3), ("i_m1", 3), ("i_m2", 3), ("i_p0", 3), ("i_p1", 3),
                        ("i_p2", 3), ("i_b0", 3), ("i_b1", 3), ("i_b2", 3), ("i_lens", 3),
                    ],
                    instanced: true,
                },
            ])?;

            let model_prog = prog("model.vert", "scene.frag", &[], false)?;
            let model_vbo = gu::buffer(&gl, &[], glow::STATIC_DRAW)?;
            let model_inst = gu::buffer(&gl, &[], glow::DYNAMIC_DRAW)?;
            let model_vao = gu::vertex_array(&gl, model_prog, &[
                Layout { buffer: model_vbo, attrs: &[("in_pos", 3), ("in_normal", 3)], instanced: false },
                Layout {
                    buffer: model_inst,
                    attrs: &[("i_c0", 3), ("i_c1", 3), ("i_c2", 3), ("i_t", 3), ("i_emissive", 3)],
                    instanced: true,
                },
            ])?;

            let cone: Vec<f32> = cone_mesh(CONE_SEGMENTS).iter().flat_map(|p| p.to_array()).collect();
            let cone_vbo = gu::buffer(&gl, &cone, glow::STATIC_DRAW)?;
            let beam_inst = gu::buffer(&gl, &[], glow::DYNAMIC_DRAW)?;
            let beam_attrs: &[(&str, i32)] = &[
                ("i_apex", 3), ("i_ax", 3), ("i_ay", 3), ("i_az", 3), ("i_cone", 3), ("i_color", 3), ("i_shadow", 4),
            ];
            let beam_vao = |p: glow::Program| {
                gu::vertex_array(&gl, p, &[
                    Layout { buffer: cone_vbo, attrs: &[("in_vert", 3)], instanced: false },
                    Layout { buffer: beam_inst, attrs: beam_attrs, instanced: true },
                ])
            };
            let surface_vao = beam_vao(surface_prog)?;
            let volume_vao = beam_vao(volume_prog)?;

            let light_image_inst = gu::buffer(&gl, &[], glow::DYNAMIC_DRAW)?;
            let light_image_vao = gu::vertex_array(&gl, light_image_prog, &[Layout {
                buffer: light_image_inst,
                attrs: &[("i_slot", 1), ("i_wheel1", 3), ("i_wheel2", 3), ("i_range1", 2), ("i_range2", 2), ("i_iris", 1),
                    ("i_color1", 3), ("i_color2", 3), ("i_split", 1)],
                instanced: true,
            }])?;
            let composite_vao = gu::vertex_array(&gl, composite_prog, &[])?;

            let albedo = gu::texture_2d(&gl, size, &gu::RGBA8, false)?;
            let normal = gu::texture_2d(&gl, size, &gu::RGBA16F, false)?;
            let light = gu::texture_2d(&gl, size, &gu::RGBA16F, false)?;
            let depth = gu::texture_2d(&gl, size, &gu::DEPTH24, false)?;
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_COMPARE_MODE, glow::NONE as i32);
            let gbuffer = gu::framebuffer(&gl, &[albedo, normal, light], Some(depth))?;
            let light_fbo = gu::framebuffer(&gl, &[light], None)?;
            let volume_tex_size = scaled(size, quality.volume_scale);
            let volume = gu::texture_2d(&gl, volume_tex_size, &gu::RGBA16F, true)?;
            let volume_fbo = gu::framebuffer(&gl, &[volume], None)?;

            Ok(Self {
                size,
                quality,
                volume_scale: quality.volume_scale,
                gobo_count,
                gobos,
                noise,
                scene_prog,
                body_prog,
                surface_prog,
                volume_prog,
                composite_prog,
                shadow_prog,
                light_image_prog,
                scene_vao,
                scene_vbo,
                scene_vertices: (scene_vertices.len() / 9) as i32,
                shadow_vao,
                body_vao,
                body_vertices: (body.len() / 7) as i32,
                body_inst,
                model_prog,
                model_vbo,
                model_inst,
                model_vao,
                model_ranges: Vec::new(),
                model_groups: Vec::new(),
                surface_vao,
                volume_vao,
                cone_vertices: (cone.len() / 3) as i32,
                // polygon inscribed in the circle -> enlarge so the mesh fully covers the analytic cone
                mesh_scale: 1.02 / (std::f32::consts::PI / CONE_SEGMENTS as f32).cos(),
                beam_inst,
                light_image_vao,
                light_image_inst,
                composite_vao,
                albedo,
                normal,
                light,
                depth,
                gbuffer,
                light_fbo,
                volume,
                volume_tex_size,
                volume_fbo,
                face_rot: face_rotations(),
                shadow: None,
                shadow_key: Vec::new(),
                light_image: None,
                light_image_count: 0,
                gl,
            })
        }
    }

    /// Replaces the gobo library and the static scene, e.g. after loading another MVR.
    pub fn set_scene(&mut self, gobos: &[Vec<u8>], scene_vertices: &[f32]) -> Result<()> {
        unsafe {
            let texture = resources::upload_gobos(&self.gl, gobos)?;
            self.gl.delete_texture(std::mem::replace(&mut self.gobos, texture));
            self.gobo_count = gobos.len();
            gu::upload(&self.gl, self.scene_vbo, scene_vertices);
            self.scene_vertices = (scene_vertices.len() / 9) as i32;
            // shadow maps contain the old scene
            self.shadow_key.clear();
        }
        Ok(())
    }

    /// Uploads the GDTF model meshes (interleaved pos 3, normal 3); mesh ids are indices into `meshes`.
    pub fn set_models(&mut self, meshes: &[Vec<f32>]) {
        let mut data = Vec::new();
        self.model_ranges.clear();
        for m in meshes {
            self.model_ranges.push(((data.len() / 6) as i32, (m.len() / 6) as i32));
            data.extend_from_slice(m);
        }
        unsafe {
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.model_vbo));
            self.gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck::cast_slice(&data), glow::STATIC_DRAW);
        }
        self.model_groups.clear();
    }

    /// Model parts to draw from the next frame on.
    pub fn set_model_instances(&mut self, instances: &[ModelInstance]) {
        for (_, data) in &mut self.model_groups {
            data.clear();
        }
        for inst in instances {
            let group = match self.model_groups.iter().position(|(m, _)| *m == inst.mesh) {
                Some(g) => g,
                None => {
                    self.model_groups.push((inst.mesh, Vec::new()));
                    self.model_groups.len() - 1
                }
            };
            let w = inst.world;
            let data = &mut self.model_groups[group].1;
            data.extend(w.x_axis.truncate().to_array());
            data.extend(w.y_axis.truncate().to_array());
            data.extend(w.z_axis.truncate().to_array());
            data.extend(w.w_axis.truncate().to_array());
            data.extend(inst.emissive.to_array());
        }
    }

    /// Follows the window size; expects both dimensions to be positive.
    pub fn resize(&mut self, size: (i32, i32)) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.volume_tex_size = scaled(size, self.quality.volume_scale);
        let gl = &self.gl;
        unsafe {
            gu::resize_texture_2d(gl, self.albedo, size, &gu::RGBA8);
            gu::resize_texture_2d(gl, self.normal, size, &gu::RGBA16F);
            gu::resize_texture_2d(gl, self.light, size, &gu::RGBA16F);
            gu::resize_texture_2d(gl, self.depth, size, &gu::DEPTH24);
            gu::resize_texture_2d(gl, self.volume, self.volume_tex_size, &gu::RGBA16F);
        }
    }

    /// Dynamic resolution for the haze pass; the quality preset is the upper limit.
    pub fn adapt_volume_scale(&mut self, fps: f32, target_fps: f32) {
        if fps < 0.95 * target_fps {
            self.volume_scale = (self.volume_scale * 0.85).max(MIN_VOLUME_SCALE);
        } else if fps > 1.3 * target_fps {
            self.volume_scale = (self.volume_scale * 1.1).min(self.quality.volume_scale);
        }
    }

    unsafe fn bind_target(&self, fbo: Option<glow::Framebuffer>, (w, h): (i32, i32)) {
        self.gl.bind_framebuffer(glow::FRAMEBUFFER, fbo);
        self.gl.viewport(0, 0, w, h);
    }

    unsafe fn new_atlas(&self, old: Option<Atlas>, tiles: i32, tile: i32, fmt: &gu::TexFormat) -> Result<Atlas> {
        let gl = &self.gl;
        if let Some(old) = old {
            gl.delete_framebuffer(old.fbo);
            gl.delete_texture(old.tex);
        }
        let (tile, per_row, size) = atlas_layout(tiles, tile, gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE));
        let tex = gu::texture_2d(gl, size, fmt, true)?;
        let fbo = gu::framebuffer(gl, &[tex], None)?;
        Ok(Atlas { tex, fbo, size, grid: [per_row as f32, tile as f32] })
    }

    /// Distance cube map per head in one atlas; only static geometry casts, so rebuild on rig/scene change.
    unsafe fn build_shadows(&mut self, fixtures: &[Fixture]) -> Result<()> {
        let old = self.shadow.take();
        let atlas = self.new_atlas(old, 6 * fixtures.len() as i32, self.quality.shadow_tile, &gu::R16F)?;
        let gl = &self.gl;
        let (per_row, tile) = (atlas.grid[0] as i32, atlas.grid[1] as i32);
        let proj = Mat4::perspective_rh_gl(90f32.to_radians(), 1.0, 0.05, 100.0);
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(atlas.fbo));
        gl.clear_buffer_f32_slice(glow::COLOR, 0, &[60000.0, 0.0, 0.0, 0.0]);
        // MIN blending keeps the nearest occluder without needing a depth buffer for the whole atlas
        gl.disable(glow::DEPTH_TEST);
        gl.disable(glow::CULL_FACE);
        gl.enable(glow::BLEND);
        gl.blend_equation(glow::MIN);
        gl.bind_vertex_array(Some(self.shadow_vao));
        for (i, f) in fixtures.iter().enumerate() {
            for (face, rot) in self.face_rot.iter().enumerate() {
                let n = 6 * i as i32 + face as i32;
                gl.viewport((n % per_row) * tile, (n / per_row) * tile, tile, tile);
                let mut view = Mat4::from_mat3(*rot);
                view.w_axis = (-(*rot * f.pos)).extend(1.0);
                gu::set(gl, self.shadow_prog, &[("u_view_proj", U::M4(proj * view)), ("u_origin", U::V3(f.pos.to_array()))]);
                gl.draw_arrays(glow::TRIANGLES, 0, self.scene_vertices);
            }
        }
        gl.blend_equation(glow::FUNC_ADD);
        gl.disable(glow::BLEND);
        self.shadow = Some(atlas);
        Ok(())
    }

    /// One tile per fixture with all wheels composed, so beam shaders need a single fetch per sample.
    unsafe fn compose_light_images(&mut self, fixtures: &[Fixture]) -> Result<()> {
        if fixtures.len() != self.light_image_count {
            let old = self.light_image.take();
            self.light_image = Some(self.new_atlas(old, fixtures.len() as i32, self.quality.light_tile, &gu::RGBA8)?);
            self.light_image_count = fixtures.len();
        }
        let gl = &self.gl;
        let atlas = self.light_image.as_ref().unwrap();
        gu::upload(gl, self.light_image_inst, &build_light_instances(fixtures));
        self.bind_target(Some(atlas.fbo), atlas.size);
        gu::bind_texture(gl, 0, glow::TEXTURE_2D_ARRAY, self.gobos);
        gu::set(gl, self.light_image_prog, &[
            ("u_gobos", U::I(0)),
            ("u_wheel_slots", U::F((self.gobo_count + 1) as f32)),
            ("u_lod_base", U::F((GOBO_SIZE as f32 / atlas.grid[1]).log2())),
            ("u_light_grid", U::V2(atlas.grid)),
            ("u_light_size", v2(atlas.size)),
        ]);
        gl.bind_vertex_array(Some(self.light_image_vao));
        gl.draw_arrays_instanced(glow::TRIANGLE_STRIP, 0, 4, fixtures.len() as i32);
        Ok(())
    }

    /// Renders one frame to the default framebuffer; returns the number of drawn beams.
    pub fn render(&mut self, fixtures: &[Fixture], eye: Vec3, view: Mat4, proj: Mat4, time: f32) -> Result<usize> {
        unsafe {
            let positions: Vec<Vec3> = fixtures.iter().map(|f| f.pos).collect();
            if positions != self.shadow_key {
                self.build_shadows(fixtures)?;
                self.shadow_key = positions;
            }
            let view_proj = proj * view;
            let beams = build_beam_instances(fixtures);
            let beam_count = beams.len() / crate::fixtures::BEAM_FLOATS;
            let bodies = build_body_instances(fixtures);
            let gl = &self.gl;

            self.bind_target(Some(self.gbuffer), self.size);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear_depth_f32(1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.disable(glow::BLEND);
            gl.disable(glow::CULL_FACE);
            gl.enable(glow::DEPTH_TEST);
            gu::set(gl, self.scene_prog, &[("u_view_proj", U::M4(view_proj))]);
            gl.bind_vertex_array(Some(self.scene_vao));
            gl.draw_arrays(glow::TRIANGLES, 0, self.scene_vertices);
            if !bodies.is_empty() {
                gu::upload(gl, self.body_inst, &bodies);
                gu::set(gl, self.body_prog, &[("u_view_proj", U::M4(view_proj))]);
                gl.bind_vertex_array(Some(self.body_vao));
                gl.draw_arrays_instanced(glow::TRIANGLES, 0, self.body_vertices, (bodies.len() / BODY_FLOATS) as i32);
            }
            if self.model_groups.iter().any(|(_, d)| !d.is_empty()) {
                gu::set(gl, self.model_prog, &[("u_view_proj", U::M4(view_proj))]);
                gl.bind_vertex_array(Some(self.model_vao));
                for (mesh, data) in &self.model_groups {
                    let Some(&(first, count)) = self.model_ranges.get(*mesh as usize) else { continue };
                    if data.is_empty() || count == 0 {
                        continue;
                    }
                    gu::upload(gl, self.model_inst, data);
                    gl.draw_arrays_instanced(glow::TRIANGLES, first, count, (data.len() / crate::fixtures::MODEL_FLOATS) as i32);
                }
            }
            gl.disable(glow::DEPTH_TEST);

            let volume_size = scaled(self.size, self.volume_scale);
            self.bind_target(Some(self.volume_fbo), volume_size);
            gl.clear(glow::COLOR_BUFFER_BIT);
            if beam_count > 0 {
                self.compose_light_images(fixtures)?;
                let gl = &self.gl;
                let light_image = self.light_image.as_ref().unwrap();
                let shadow = self.shadow.as_ref().unwrap();
                gu::upload(gl, self.beam_inst, &beams);
                gl.enable(glow::BLEND);
                gl.enable(glow::CULL_FACE);
                gl.blend_func(glow::ONE, glow::ONE);
                // cull outside-facing triangles: only cone back faces -> exactly one fragment per covered pixel
                gl.front_face(glow::CW);
                gu::bind_texture(gl, 0, glow::TEXTURE_2D, self.depth);
                gu::bind_texture(gl, 1, glow::TEXTURE_2D, light_image.tex);
                gu::bind_texture(gl, 2, glow::TEXTURE_2D, self.albedo);
                gu::bind_texture(gl, 3, glow::TEXTURE_2D, self.normal);
                gu::bind_texture(gl, 4, glow::TEXTURE_3D, self.noise);
                gu::bind_texture(gl, 5, glow::TEXTURE_2D, shadow.tex);
                let faces: Vec<f32> = self.face_rot.iter().flat_map(|m| m.to_cols_array()).collect();
                let shared = [
                    ("u_view_proj", U::M4(view_proj)),
                    ("u_inv_view_proj", U::M4(view_proj.inverse())),
                    ("u_cam_pos", U::V3(eye.to_array())),
                    ("u_mesh_scale", U::F(self.mesh_scale)),
                    ("u_depth", U::I(0)),
                    ("u_light_image", U::I(1)),
                    ("u_light_grid", U::V2(light_image.grid)),
                    ("u_light_size", v2(light_image.size)),
                    ("u_shadow", U::I(5)),
                    ("u_shadow_faces", U::M3s(&faces)),
                    ("u_shadow_grid", U::V2(shadow.grid)),
                    ("u_shadow_size", v2(shadow.size)),
                ];

                self.bind_target(Some(self.light_fbo), self.size);
                gu::set(gl, self.surface_prog, &shared);
                gu::set(gl, self.surface_prog, &[
                    ("u_target_size", v2(self.size)),
                    ("u_albedo", U::I(2)),
                    ("u_normal", U::I(3)),
                    ("u_gain", U::F(SURFACE_GAIN)),
                ]);
                gl.bind_vertex_array(Some(self.surface_vao));
                gl.draw_arrays_instanced(glow::TRIANGLES, 0, self.cone_vertices, beam_count as i32);

                self.bind_target(Some(self.volume_fbo), volume_size);
                let q = self.quality;
                gu::set(gl, self.volume_prog, &shared);
                gu::set(gl, self.volume_prog, &[
                    ("u_target_size", v2(volume_size)),
                    ("u_noise", U::I(4)),
                    ("u_steps", U::I(q.steps)),
                    ("u_noise_amount", U::F(q.noise)),
                    ("u_time", U::F(time)),
                    ("u_haze", U::F(HAZE_DENSITY)),
                    ("u_gain", U::F(VOLUME_GAIN)),
                    ("u_extinction", U::F(EXTINCTION)),
                    ("u_g", U::F(HG_G)),
                ]);
                gl.bind_vertex_array(Some(self.volume_vao));
                gl.draw_arrays_instanced(glow::TRIANGLES, 0, self.cone_vertices, beam_count as i32);

                gl.front_face(glow::CCW);
                gl.disable(glow::BLEND);
                gl.disable(glow::CULL_FACE);
            }

            let gl = &self.gl;
            self.bind_target(None, self.size);
            gu::bind_texture(gl, 0, glow::TEXTURE_2D, self.albedo);
            gu::bind_texture(gl, 1, glow::TEXTURE_2D, self.light);
            gu::bind_texture(gl, 2, glow::TEXTURE_2D, self.volume);
            let (tw, th) = (self.volume_tex_size.0 as f32, self.volume_tex_size.1 as f32);
            let (vw, vh) = (volume_size.0 as f32, volume_size.1 as f32);
            gu::set(gl, self.composite_prog, &[
                ("u_albedo", U::I(0)),
                ("u_light", U::I(1)),
                ("u_volume", U::I(2)),
                ("u_ambient", U::F(AMBIENT)),
                ("u_volume_uv_scale", U::V2([vw / tw, vh / th])),
                ("u_volume_uv_max", U::V2([(vw - 0.5) / tw, (vh - 0.5) / th])),
            ]);
            gl.bind_vertex_array(Some(self.composite_vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            Ok(beam_count)
        }
    }

    /// Reads the default framebuffer as top-down RGB.
    pub fn read_screen(&self) -> Vec<u8> {
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let mut buf = vec![0u8; w * h * 3];
        unsafe {
            self.gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            self.gl.read_pixels(
                0, 0, w as i32, h as i32, glow::RGB, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut buf)),
            );
        }
        buf.chunks(w * 3).rev().flatten().copied().collect()
    }

    pub fn finish(&self) {
        unsafe { self.gl.finish() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_layout_fits_or_halves_tiles() {
        assert_eq!(atlas_layout(200, 512, 16384), (512, 16, (8192, 6656)));
        assert_eq!(atlas_layout(1200, 256, 16384), (128, 64, (8192, 2432)));
        assert_eq!(atlas_layout(6, 128, 16384), (128, 6, (768, 128)));
    }
}
