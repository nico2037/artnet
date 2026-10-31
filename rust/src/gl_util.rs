use anyhow::{anyhow, Result};
use glam::Mat4;
use glow::HasContext;

const COMMON: &str = include_str!("../../shaders/common.glsl");

/// Shaders are shared with the Python prototype and embedded at compile time.
fn shader_file(name: &str) -> &'static str {
    match name {
        "scene.vert" => include_str!("../../shaders/scene.vert"),
        "scene.frag" => include_str!("../../shaders/scene.frag"),
        "fixture.vert" => include_str!("../../shaders/fixture.vert"),
        "model.vert" => include_str!("../../shaders/model.vert"),
        "beam.vert" => include_str!("../../shaders/beam.vert"),
        "beam.frag" => include_str!("../../shaders/beam.frag"),
        "fullscreen.vert" => include_str!("../../shaders/fullscreen.vert"),
        "composite.frag" => include_str!("../../shaders/composite.frag"),
        "shadow.vert" => include_str!("../../shaders/shadow.vert"),
        "shadow.frag" => include_str!("../../shaders/shadow.frag"),
        "lightimage.vert" => include_str!("../../shaders/lightimage.vert"),
        "lightimage.frag" => include_str!("../../shaders/lightimage.frag"),
        _ => panic!("unknown shader {name}"),
    }
}

fn source(name: &str, defines: &[&str], common: bool) -> String {
    let mut s = String::from("#version 330 core\n");
    for d in defines {
        s += &format!("#define {d}\n");
    }
    if common {
        s += COMMON;
    }
    s + shader_file(name)
}

pub unsafe fn program(gl: &glow::Context, vs: &str, fs: &str, defines: &[&str], common: bool) -> Result<glow::Program> {
    let prog = gl.create_program().map_err(|e| anyhow!(e))?;
    let mut shaders = Vec::new();
    for (kind, name, src) in [
        (glow::VERTEX_SHADER, vs, source(vs, &[], false)),
        (glow::FRAGMENT_SHADER, fs, source(fs, defines, common)),
    ] {
        let shader = gl.create_shader(kind).map_err(|e| anyhow!(e))?;
        gl.shader_source(shader, &src);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            return Err(anyhow!("{name} {defines:?}: {}", gl.get_shader_info_log(shader)));
        }
        gl.attach_shader(prog, shader);
        shaders.push(shader);
    }
    gl.link_program(prog);
    if !gl.get_program_link_status(prog) {
        return Err(anyhow!("link {vs} + {fs}: {}", gl.get_program_info_log(prog)));
    }
    for shader in shaders {
        gl.detach_shader(prog, shader);
        gl.delete_shader(shader);
    }
    Ok(prog)
}

pub enum U<'a> {
    I(i32),
    F(f32),
    V2([f32; 2]),
    V3([f32; 3]),
    M4(Mat4),
    M3s(&'a [f32]),
}

/// Binds the program and sets uniforms; names the compiler optimized away are skipped.
pub unsafe fn set(gl: &glow::Context, prog: glow::Program, uniforms: &[(&str, U)]) {
    gl.use_program(Some(prog));
    for (name, value) in uniforms {
        let Some(loc) = gl.get_uniform_location(prog, name) else { continue };
        let loc = Some(&loc);
        match value {
            U::I(v) => gl.uniform_1_i32(loc, *v),
            U::F(v) => gl.uniform_1_f32(loc, *v),
            U::V2(v) => gl.uniform_2_f32(loc, v[0], v[1]),
            U::V3(v) => gl.uniform_3_f32(loc, v[0], v[1], v[2]),
            U::M4(m) => gl.uniform_matrix_4_f32_slice(loc, false, &m.to_cols_array()),
            U::M3s(m) => gl.uniform_matrix_3_f32_slice(loc, false, m),
        }
    }
}

/// One vertex buffer with its attributes (name, float count); an empty name is padding.
pub struct Layout<'a> {
    pub buffer: glow::Buffer,
    pub attrs: &'a [(&'a str, i32)],
    pub instanced: bool,
}

pub unsafe fn vertex_array(gl: &glow::Context, prog: glow::Program, layouts: &[Layout]) -> Result<glow::VertexArray> {
    let vao = gl.create_vertex_array().map_err(|e| anyhow!(e))?;
    gl.bind_vertex_array(Some(vao));
    for layout in layouts {
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(layout.buffer));
        let stride = layout.attrs.iter().map(|a| a.1).sum::<i32>() * 4;
        let mut offset = 0;
        for &(name, size) in layout.attrs {
            if let Some(loc) = (!name.is_empty()).then(|| gl.get_attrib_location(prog, name)).flatten() {
                gl.enable_vertex_attrib_array(loc);
                gl.vertex_attrib_pointer_f32(loc, size, glow::FLOAT, false, stride, offset);
                if layout.instanced {
                    gl.vertex_attrib_divisor(loc, 1);
                }
            }
            offset += size * 4;
        }
    }
    gl.bind_vertex_array(None);
    Ok(vao)
}

pub unsafe fn buffer(gl: &glow::Context, data: &[f32], usage: u32) -> Result<glow::Buffer> {
    let buf = gl.create_buffer().map_err(|e| anyhow!(e))?;
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(buf));
    gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck::cast_slice(data), usage);
    Ok(buf)
}

/// Re-specifies the whole buffer each time, which lets the driver orphan the old storage.
pub unsafe fn upload(gl: &glow::Context, buf: glow::Buffer, data: &[f32]) {
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(buf));
    gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck::cast_slice(data), glow::DYNAMIC_DRAW);
}

pub struct TexFormat {
    pub internal: u32,
    pub format: u32,
    pub ty: u32,
}

pub const RGBA8: TexFormat = TexFormat { internal: glow::RGBA8, format: glow::RGBA, ty: glow::UNSIGNED_BYTE };
pub const RGBA16F: TexFormat = TexFormat { internal: glow::RGBA16F, format: glow::RGBA, ty: glow::FLOAT };
pub const R16F: TexFormat = TexFormat { internal: glow::R16F, format: glow::RED, ty: glow::FLOAT };
pub const DEPTH24: TexFormat =
    TexFormat { internal: glow::DEPTH_COMPONENT24, format: glow::DEPTH_COMPONENT, ty: glow::UNSIGNED_INT };

/// Replaces the storage only, so framebuffers the texture is attached to stay valid.
pub unsafe fn resize_texture_2d(gl: &glow::Context, tex: glow::Texture, (w, h): (i32, i32), fmt: &TexFormat) {
    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
    gl.tex_image_2d(
        glow::TEXTURE_2D, 0, fmt.internal as i32, w, h, 0, fmt.format, fmt.ty, glow::PixelUnpackData::Slice(None),
    );
}

pub unsafe fn texture_2d(gl: &glow::Context, size: (i32, i32), fmt: &TexFormat, linear: bool) -> Result<glow::Texture> {
    let tex = gl.create_texture().map_err(|e| anyhow!(e))?;
    resize_texture_2d(gl, tex, size, fmt);
    let filter = if linear { glow::LINEAR } else { glow::NEAREST } as i32;
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, filter);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, filter);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    Ok(tex)
}

pub unsafe fn framebuffer(gl: &glow::Context, colors: &[glow::Texture], depth: Option<glow::Texture>) -> Result<glow::Framebuffer> {
    let fb = gl.create_framebuffer().map_err(|e| anyhow!(e))?;
    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fb));
    for (i, tex) in colors.iter().enumerate() {
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0 + i as u32, glow::TEXTURE_2D, Some(*tex), 0);
    }
    if let Some(tex) = depth {
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::DEPTH_ATTACHMENT, glow::TEXTURE_2D, Some(tex), 0);
    }
    let attachments: Vec<u32> = (0..colors.len() as u32).map(|i| glow::COLOR_ATTACHMENT0 + i).collect();
    gl.draw_buffers(&attachments);
    let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
    if status != glow::FRAMEBUFFER_COMPLETE {
        return Err(anyhow!("framebuffer incomplete: 0x{status:x}"));
    }
    Ok(fb)
}

pub unsafe fn bind_texture(gl: &glow::Context, unit: u32, target: u32, tex: glow::Texture) {
    gl.active_texture(glow::TEXTURE0 + unit);
    gl.bind_texture(target, Some(tex));
}
