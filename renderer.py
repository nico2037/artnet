"""Deferred renderer: one instanced cone per beam (light volume), so cost scales with beam screen area."""
import math
from pathlib import Path

import moderngl
import numpy as np
from PIL import Image

from fixtures import (BEAM_FLOATS, BODY_FLOATS, LIGHT_FLOATS, build_beam_instances, build_body_instances,
                      build_light_instances)
from mathutil import cone_mesh, fixture_body_mesh, look_at, perspective

SHADERS = Path(__file__).resolve().parent / "shaders"
CONE_SEGMENTS = 24

QUALITY = {
    "low": {"volume_scale": 0.33, "steps": 12, "noise": 0.0, "shadow_tile": 128, "light_tile": 256},
    "medium": {"volume_scale": 0.5, "steps": 24, "noise": 1.0, "shadow_tile": 128, "light_tile": 256},
    "high": {"volume_scale": 0.75, "steps": 48, "noise": 1.0, "shadow_tile": 256, "light_tile": 512},
}

HAZE_DENSITY = 0.1
SURFACE_GAIN = 50.0
VOLUME_GAIN = 60.0
AMBIENT = 0.006
EXTINCTION = 0.02
HG_G = 0.35

# cube faces in the order the shader selects them: +x -x +y -y +z -z
_FACE_ROT = [look_at((0, 0, 0), d, up)[:3, :3] for d, up in (
    ((1, 0, 0), (0, -1, 0)), ((-1, 0, 0), (0, -1, 0)), ((0, 1, 0), (0, 0, 1)),
    ((0, -1, 0), (0, 0, -1)), ((0, 0, 1), (0, -1, 0)), ((0, 0, -1), (0, -1, 0)))]
_FACE_PROJ = perspective(90.0, 1.0, 0.05, 100.0)
_ATLAS_MAX = 8192
GOBO_SIZE = 256
MIN_VOLUME_SCALE = 0.2


def _atlas_layout(ctx, tiles, tile):
    """Halves the tile size until all tiles fit; returns (tile, tiles per row, atlas size)."""
    max_size = min(_ATLAS_MAX, ctx.info["GL_MAX_TEXTURE_SIZE"])
    while tile > 16 and tiles > (max_size // tile) ** 2:
        tile //= 2
    per_row = min(tiles, max_size // tile)
    return tile, per_row, (per_row * tile, math.ceil(tiles / per_row) * tile)


def _source(name, defines=(), common=False):
    head = "#version 330 core\n" + "".join(f"#define {d}\n" for d in defines)
    if common:
        head += (SHADERS / "common.glsl").read_text()
    return head + (SHADERS / name).read_text()


def _mat(m):
    return np.ascontiguousarray(np.asarray(m, np.float32).T)


def _set(prog, **uniforms):
    for name, value in uniforms.items():
        u = prog.get(name, None)
        if u is None:
            continue
        if isinstance(value, np.ndarray):
            u.write(value.astype(np.float32).tobytes())
        else:
            u.value = value


def _load_gobos(ctx, folder, size=GOBO_SIZE):
    files = sorted(Path(folder).glob("*.png"))
    layers = [np.asarray(Image.open(f).convert("L").resize((size, size), Image.LANCZOS), np.uint8) for f in files]
    if not layers:
        layers = [np.full((size, size), 255, np.uint8)]
    tex = ctx.texture_array((size, size, len(layers)), 1, np.stack(layers).tobytes())
    tex.build_mipmaps()
    tex.filter = (moderngl.LINEAR_MIPMAP_LINEAR, moderngl.LINEAR)
    tex.repeat_x = tex.repeat_y = False
    return tex, len(files)


def _noise_texture(ctx, n=32, sigma=1.2, seed=1):
    v = np.random.default_rng(seed).random((n, n, n))
    k = np.fft.fftfreq(n)
    kx, ky, kz = np.meshgrid(k, k, k, indexing="ij")
    v = np.real(np.fft.ifftn(np.fft.fftn(v) * np.exp(-0.5 * (2 * np.pi * sigma) ** 2 * (kx**2 + ky**2 + kz**2))))
    v = (v - v.min()) / (v.max() - v.min())
    tex = ctx.texture3d((n, n, n), 1, (v * 255).astype(np.uint8).tobytes())
    tex.repeat_x = tex.repeat_y = tex.repeat_z = True
    return tex


class Renderer:
    def __init__(self, ctx, size, quality, gobo_dir, scene_vertices):
        self.ctx = ctx
        self.size = tuple(size)
        self.quality = QUALITY[quality]
        self.gobos, self.gobo_count = _load_gobos(ctx, gobo_dir)
        self.noise = _noise_texture(ctx)

        program = lambda vs, fs, defines=(), common=False: ctx.program(
            vertex_shader=_source(vs), fragment_shader=_source(fs, defines, common))
        self.scene_prog = program("scene.vert", "scene.frag")
        self.body_prog = program("fixture.vert", "scene.frag")
        self.surface_prog = program("beam.vert", "beam.frag", ["SURFACE"], True)
        self.volume_prog = program("beam.vert", "beam.frag", ["VOLUME"], True)
        self.composite_prog = program("fullscreen.vert", "composite.frag")
        self.shadow_prog = program("shadow.vert", "shadow.frag")
        self.light_image_prog = program("lightimage.vert", "lightimage.frag")

        scene_vbo = ctx.buffer(np.asarray(scene_vertices, np.float32).tobytes())
        self.scene_vao = ctx.vertex_array(
            self.scene_prog, [(scene_vbo, "3f 3f 3f", "in_pos", "in_normal", "in_albedo")])
        self.shadow_vao = ctx.vertex_array(self.shadow_prog, [(scene_vbo, "3f 24x", "in_pos")])
        self.shadow_atlas = None
        self._shadow_key = None

        self.light_image_inst = ctx.buffer(reserve=LIGHT_FLOATS * 4 * 16, dynamic=True)
        self.light_image_vao = ctx.vertex_array(self.light_image_prog, [
            (self.light_image_inst, "1f 3f 3f/i", "i_slot", "i_wheel1", "i_wheel2")])
        self.light_image = None
        self._light_image_count = 0

        body = fixture_body_mesh()
        self.body_vertices = len(body)
        self.body_inst = ctx.buffer(reserve=BODY_FLOATS * 4 * 16, dynamic=True)
        self.body_vao = ctx.vertex_array(self.body_prog, [
            (ctx.buffer(body.astype(np.float32).tobytes()), "3f 3f 1f", "in_pos", "in_normal", "in_part"),
            (self.body_inst, " ".join(["3f"] * 11) + "/i", "i_pos", "i_m0", "i_m1", "i_m2",
             "i_p0", "i_p1", "i_p2", "i_b0", "i_b1", "i_b2", "i_lens"),
        ])

        cone = cone_mesh(CONE_SEGMENTS)
        self.cone_vertices = len(cone)
        # polygon inscribed in the circle -> enlarge so the mesh fully covers the analytic cone
        self.mesh_scale = 1.02 / math.cos(math.pi / CONE_SEGMENTS)
        cone_vbo = ctx.buffer(cone.astype(np.float32).tobytes())
        self.beam_inst = ctx.buffer(reserve=BEAM_FLOATS * 4 * 64, dynamic=True)
        beam_layout = (self.beam_inst, "3f 3f 3f 3f 3f 3f 4f/i",
                       "i_apex", "i_ax", "i_ay", "i_az", "i_cone", "i_color", "i_shadow")
        self.surface_vao = ctx.vertex_array(self.surface_prog, [(cone_vbo, "3f", "in_vert"), beam_layout])
        self.volume_vao = ctx.vertex_array(self.volume_prog, [(cone_vbo, "3f", "in_vert"), beam_layout])
        self.composite_vao = ctx.vertex_array(self.composite_prog, [])

        self._create_targets()

    def _create_targets(self):
        ctx, (w, h) = self.ctx, self.size
        self.albedo = ctx.texture((w, h), 4, dtype="f1")
        self.normal = ctx.texture((w, h), 4, dtype="f2")
        self.light = ctx.texture((w, h), 4, dtype="f2")
        self.depth = ctx.depth_texture((w, h))
        for tex in (self.albedo, self.normal, self.light, self.depth):
            tex.filter = (moderngl.NEAREST, moderngl.NEAREST)
        self.depth.compare_func = ""
        self.gbuffer = ctx.framebuffer([self.albedo, self.normal, self.light], self.depth)
        self.light_fbo = ctx.framebuffer([self.light])

        s = self.quality["volume_scale"]
        self.volume_scale = s
        self.volume = ctx.texture((max(1, int(w * s)), max(1, int(h * s))), 4, dtype="f2")
        self.volume.repeat_x = self.volume.repeat_y = False
        self.volume_fbo = ctx.framebuffer([self.volume])

    def adapt_volume_scale(self, fps, target_fps):
        """Dynamic resolution for the haze pass; the quality preset is the upper limit."""
        if fps < 0.95 * target_fps:
            self.volume_scale = max(MIN_VOLUME_SCALE, self.volume_scale * 0.85)
        elif fps > 1.3 * target_fps:
            self.volume_scale = min(self.quality["volume_scale"], self.volume_scale * 1.1)

    @staticmethod
    def _upload(buffer, data):
        if data.nbytes > buffer.size:
            buffer.orphan(data.nbytes * 2)
        buffer.write(data.tobytes())

    def _build_shadows(self, positions):
        """Distance cube map per head in one atlas; only static geometry casts, so rebuild on rig/scene change."""
        ctx = self.ctx
        tile, per_row, size = _atlas_layout(ctx, 6 * len(positions), self.quality["shadow_tile"])

        if self.shadow_atlas is not None:
            self.shadow_atlas.release()
        self.shadow_atlas = ctx.texture(size, 1, dtype="f2")
        self.shadow_atlas.repeat_x = self.shadow_atlas.repeat_y = False
        fbo = ctx.framebuffer([self.shadow_atlas])
        fbo.use()
        fbo.clear(60000.0)
        # MIN blending keeps the nearest occluder without needing a depth buffer for the whole atlas
        ctx.disable(moderngl.DEPTH_TEST | moderngl.CULL_FACE)
        ctx.enable(moderngl.BLEND)
        ctx.blend_equation = moderngl.MIN
        for i, origin in enumerate(positions.astype(np.float64)):
            view = np.eye(4)
            for face, rot in enumerate(_FACE_ROT):
                n = 6 * i + face
                ctx.viewport = ((n % per_row) * tile, (n // per_row) * tile, tile, tile)
                view[:3, :3], view[:3, 3] = rot, -rot @ origin
                _set(self.shadow_prog, u_view_proj=_mat(_FACE_PROJ @ view), u_origin=tuple(origin))
                self.shadow_vao.render(moderngl.TRIANGLES)
        ctx.blend_equation = moderngl.FUNC_ADD
        ctx.disable(moderngl.BLEND)
        fbo.release()
        self.shadow_grid = (float(per_row), float(tile))

    def _compose_light_images(self, fixtures):
        """One tile per fixture with all wheels composed, so beam shaders need a single fetch per sample."""
        ctx = self.ctx
        if len(fixtures) != self._light_image_count:
            tile, per_row, size = _atlas_layout(ctx, len(fixtures), self.quality["light_tile"])
            if self.light_image is not None:
                self.light_image.release()
                self.light_image_fbo.release()
            self.light_image = ctx.texture(size, 1, dtype="f1")
            self.light_image.repeat_x = self.light_image.repeat_y = False
            self.light_image_fbo = ctx.framebuffer([self.light_image])
            self.light_image_grid = (float(per_row), float(tile))
            self._light_image_count = len(fixtures)

        self._upload(self.light_image_inst, build_light_instances(fixtures))
        self.light_image_fbo.use()
        self.gobos.use(0)
        _set(self.light_image_prog, u_gobos=0, u_wheel_slots=float(self.gobo_count + 1),
             u_lod_base=math.log2(GOBO_SIZE / self.light_image_grid[1]),
             u_light_grid=self.light_image_grid, u_light_size=tuple(map(float, self.light_image.size)))
        self.light_image_vao.render(moderngl.TRIANGLE_STRIP, vertices=4, instances=len(fixtures))

    def render(self, fixtures, eye, view, proj, time):
        """Renders one frame to the screen; returns the number of drawn beams."""
        ctx = self.ctx
        view_proj = proj @ view
        shadow_key = fixtures["pos"].tobytes()
        if shadow_key != self._shadow_key:
            self._build_shadows(fixtures["pos"])
            self._shadow_key = shadow_key
        beams = build_beam_instances(fixtures)
        bodies = build_body_instances(fixtures)

        self.gbuffer.use()
        self.gbuffer.clear(0.0, 0.0, 0.0, 0.0, depth=1.0)
        ctx.disable(moderngl.BLEND | moderngl.CULL_FACE)
        ctx.enable(moderngl.DEPTH_TEST)
        _set(self.scene_prog, u_view_proj=_mat(view_proj))
        self.scene_vao.render(moderngl.TRIANGLES)
        if len(bodies):
            self._upload(self.body_inst, bodies)
            _set(self.body_prog, u_view_proj=_mat(view_proj))
            self.body_vao.render(moderngl.TRIANGLES, vertices=self.body_vertices, instances=len(bodies))
        ctx.disable(moderngl.DEPTH_TEST)

        w, h = self.size
        volume_size = (max(1, int(w * self.volume_scale)), max(1, int(h * self.volume_scale)))
        self.volume_fbo.viewport = (0, 0, *volume_size)
        self.volume_fbo.use()
        self.volume_fbo.clear(0.0, 0.0, 0.0, 0.0)
        if len(beams):
            self._compose_light_images(fixtures)
            self._upload(self.beam_inst, beams)
            ctx.enable(moderngl.BLEND | moderngl.CULL_FACE)
            ctx.blend_func = moderngl.ONE, moderngl.ONE
            # cull outside-facing triangles: only cone back faces -> exactly one fragment per covered pixel
            ctx.front_face = "cw"
            self.depth.use(0)
            self.light_image.use(1)
            self.albedo.use(2)
            self.normal.use(3)
            self.noise.use(4)
            self.shadow_atlas.use(5)
            shared = {
                "u_view_proj": _mat(view_proj),
                "u_inv_view_proj": _mat(np.linalg.inv(view_proj)),
                "u_cam_pos": tuple(float(v) for v in eye),
                "u_mesh_scale": self.mesh_scale,
                "u_depth": 0,
                "u_light_image": 1,
                "u_light_grid": self.light_image_grid,
                "u_light_size": tuple(map(float, self.light_image.size)),
                "u_shadow": 5,
                "u_shadow_faces": np.stack([_mat(r) for r in _FACE_ROT]),
                "u_shadow_grid": self.shadow_grid,
                "u_shadow_size": tuple(map(float, self.shadow_atlas.size)),
            }

            self.light_fbo.use()
            _set(self.surface_prog, **shared, u_target_size=tuple(map(float, self.size)),
                 u_albedo=2, u_normal=3, u_gain=SURFACE_GAIN)
            self.surface_vao.render(moderngl.TRIANGLES, vertices=self.cone_vertices, instances=len(beams))

            self.volume_fbo.use()
            q = self.quality
            _set(self.volume_prog, **shared, u_target_size=tuple(map(float, volume_size)),
                 u_noise=4, u_steps=q["steps"], u_noise_amount=q["noise"], u_time=float(time),
                 u_haze=HAZE_DENSITY, u_gain=VOLUME_GAIN, u_extinction=EXTINCTION, u_g=HG_G)
            self.volume_vao.render(moderngl.TRIANGLES, vertices=self.cone_vertices, instances=len(beams))

            ctx.front_face = "ccw"
            ctx.disable(moderngl.BLEND | moderngl.CULL_FACE)

        ctx.screen.use()
        self.albedo.use(0)
        self.light.use(1)
        self.volume.use(2)
        tex_w, tex_h = self.volume.size
        _set(self.composite_prog, u_albedo=0, u_light=1, u_volume=2, u_ambient=AMBIENT,
             u_volume_uv_scale=(volume_size[0] / tex_w, volume_size[1] / tex_h),
             u_volume_uv_max=((volume_size[0] - 0.5) / tex_w, (volume_size[1] - 0.5) / tex_h))
        self.composite_vao.render(moderngl.TRIANGLES, vertices=3)
        return len(beams)
