//! A loaded MVR rig driven by DMX: fixture types and profiles per (GDTF, mode), motion per fixture,
//! and the conversion to the renderer's fixture state and scene geometry.

use std::collections::HashMap;
use std::f32::consts::PI;

use anyhow::{anyhow, Context, Result};
use glam::{Mat3, Mat4, Vec3};

use crate::dmx::resolve::{Feature, Profile, Target};
use crate::fixture_type::{Beam, BeamKind, FixtureType, GoboLibrary};
use crate::fixtures::{Fixture, Pose};
use crate::io::mvr::{self, MeshInstance, Scene};
use crate::io::gdtf;
use crate::meshes::{interleave, Mesh};
use crate::sim::motion::{Actual, Motion, WheelState};

pub struct FixtureKind {
    pub ty: FixtureType,
    pub profile: Profile,
}

pub struct RigFixture {
    pub mvr: mvr::Fixture,
    /// Index into `Rig::kinds`.
    pub kind: usize,
    pub target: Target,
    pub motion: Motion,
    pub actual: Option<Actual>,
}

pub struct Rig {
    pub fixtures: Vec<RigFixture>,
    pub kinds: Vec<FixtureKind>,
    pub gobos: GoboLibrary,
    /// Fixtures left out, with the reason.
    pub skipped: Vec<String>,
    /// Fixture types loaded with a substitute, e.g. a default beam for GDTFs without beam geometry.
    pub notes: Vec<String>,
}

/// Placeholder GDTFs (e.g. exported by visualisers) often have a dimmer but no beam geometry;
/// they get a generic spot from their root part so the patch still shows light.
const FALLBACK_BEAM: Beam =
    Beam { part: 0, kind: BeamKind::Spot, beam_angle: 25.0, field_angle: 25.0, radius: 0.05, luminous_flux: 0.0 };

/// GDTF axes (Z-up) into the fixture's local frame (Y-up): (x, y, z) → (x, z, −y).
fn gdtf_to_local() -> Mat4 {
    Mat4::from_rotation_x(-PI / 2.0)
}

impl Rig {
    pub fn from_scene(scene: &Scene) -> Self {
        let mut gobos = GoboLibrary::default();
        let mut kinds = Vec::new();
        let mut by_key: HashMap<(String, String), Result<usize, String>> = HashMap::new();
        let mut fixtures = Vec::new();
        let mut skipped = Vec::new();
        let mut notes = Vec::new();
        for f in &scene.fixtures {
            let key = (f.gdtf_spec.clone(), f.gdtf_mode.clone());
            let kind = by_key
                .entry(key)
                .or_insert_with(|| {
                    let mut build = || -> Result<FixtureKind> {
                        let bytes = scene.gdtf_bytes(f).ok_or_else(|| anyhow!("{} missing in MVR", f.gdtf_spec))?;
                        let mut file = gdtf::load(bytes.to_vec())?;
                        let mut ty = FixtureType::from_gdtf(&mut file, &f.gdtf_mode, &mut gobos)?;
                        let profile = Profile::from_gdtf(&file, &f.gdtf_mode, &ty)?;
                        let has_dimmer = profile.channels.iter().flat_map(|c| &c.functions).any(|f| f.feature == Feature::Dimmer);
                        if ty.beam.is_none() && has_dimmer && !ty.parts.is_empty() {
                            ty.beam = Some(FALLBACK_BEAM);
                            notes.push(format!("{}: no beam geometry, using a {}° spot", f.gdtf_spec, FALLBACK_BEAM.beam_angle));
                        }
                        Ok(FixtureKind { ty, profile })
                    };
                    build().map(|k| {
                        kinds.push(k);
                        kinds.len() - 1
                    })
                    .map_err(|e| format!("{e:#}"))
                })
                .clone();
            match kind {
                Ok(kind) => {
                    let p = &kinds[kind].profile;
                    let target = p.resolve(&p.read(&f.addresses, |_| None), f.invert_pan, f.invert_tilt);
                    let motion = Motion::new(&target);
                    fixtures.push(RigFixture { mvr: f.clone(), kind, target, motion, actual: None });
                }
                Err(e) => skipped.push(format!("{} ({}): {e}", f.name, f.fixture_id)),
            }
        }
        Self { fixtures, kinds, gobos, skipped, notes }
    }

    /// MVR universes the patch uses (1-based).
    pub fn universes(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self.fixtures.iter().flat_map(|f| f.mvr.addresses.iter().map(|a| a.universe)).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Reads DMX (MVR universe → 512 bytes), resolves targets and advances the motion model.
    pub fn update<'a>(&mut self, dmx: impl Fn(u32) -> Option<&'a [u8; 512]>, dt: f32) {
        for f in &mut self.fixtures {
            let kind = &self.kinds[f.kind];
            let raw = kind.profile.read(&f.mvr.addresses, &dmx);
            f.target = kind.profile.resolve(&raw, f.mvr.invert_pan, f.mvr.invert_tilt);
            f.actual = Some(f.motion.step(&f.target, &kind.ty, dt));
        }
    }

    pub fn render_fixtures(&self) -> Vec<Fixture> {
        self.fixtures.iter().map(|f| self.render_fixture(f)).collect()
    }

    fn render_fixture(&self, f: &RigFixture) -> Fixture {
        let ty = &self.kinds[f.kind].ty;
        let a = f.actual.as_ref();
        let (pan, tilt) = a.map_or((f.target.pan, f.target.tilt), |a| (a.pan, a.tilt));
        let (pose, pivot) = pose(ty, f.mvr.transform, pan, tilt);
        let beam = ty.beam;
        let wash = beam.is_some_and(|b| b.kind != BeamKind::Spot);
        let default_angle = beam.map_or(25.0, |b| b.beam_angle.max(1.0));

        let mut out = Fixture { pos: pivot, pose: Some(pose), ..Fixture::default() };
        let Some(a) = a else {
            out.dimmer = 0.0;
            return out;
        };
        out.dimmer = if beam.is_some() { a.intensity } else { 0.0 };
        out.zoom = a.zoom.unwrap_or(default_angle).clamp(0.5, 120.0);
        // the iris cuts the gate image: a narrower cone showing the image's centre
        let aperture = a.iris.clamp(0.05, 1.0);
        if aperture < 1.0 {
            out.iris = 1.0 - aperture;
            out.zoom = (2.0 * (aperture * (out.zoom.to_radians() / 2.0).tan()).atan()).to_degrees().max(0.5);
        }
        // the renderer's focus sets edge softness and gobo blur; frost and wash optics soften both
        out.focus = if wash { 1.0 } else { (a.focus + a.frost).min(1.0) };

        let mut color = a.color;
        for w in &a.color_wheels {
            if let Some(slot) = w.wheel.and_then(|i| ty.wheels.get(i)).and_then(|wheel| slot_at(wheel.slots.len(), w)).and_then(|s| ty.wheels[w.wheel.unwrap()].slots.get(s)) {
                color *= slot.color;
            }
        }
        out.color = color;

        // the renderer has two gobo planes: the first two GDTF gobo wheels with images
        let mut planes = a.gobos.iter().filter_map(|w| {
            let wheel = ty.wheels.get(w.wheel?)?;
            Some((w, wheel.layers?, wheel.slots.len() as u32))
        });
        if let Some((w, base, n)) = planes.next() {
            (out.gobo, out.gobo_rot, out.gobo_range) = (w.position - 1.0, w.angle, Some((base, n)));
        }
        if let Some((w, base, n)) = planes.next() {
            (out.gobo2, out.gobo2_rot, out.gobo2_range) = (w.position - 1.0, w.angle, Some((base, n)));
        }

        // first prism whose current slot has facets
        for w in &a.prisms {
            let Some(wheel) = w.wheel.and_then(|i| ty.wheels.get(i)) else { continue };
            let Some(slot) = slot_at(wheel.slots.len(), w).and_then(|s| wheel.slots.get(s)) else { continue };
            if slot.facets.len() > 1 {
                out.prism = slot.facets.len() as u32;
                out.prism_rot = w.angle;
                // facet offset in aperture radii, as a deflection of the beam half angle
                let offset = slot.facets.iter().map(|f| f.transform.z_axis.truncate().length()).fold(0.0, f32::max);
                out.prism_spread = if offset > 0.0 { offset * out.zoom * 0.5 } else { 7.0 };
                break;
            }
        }
        out
    }
}

/// 0-based slot the wheel currently shows (nearest while travelling).
fn slot_at(slots: usize, w: &WheelState) -> Option<usize> {
    (slots > 0).then(|| (w.position - 1.0).round().rem_euclid(slots as f32) as usize)
}

/// Kinematics from the GDTF geometry chain: pan rotates its axis part about local Z, tilt about local X.
/// Returns the renderer pose and the tilt pivot (shadow origin, fixed while moving).
pub fn pose(ty: &FixtureType, mvr_transform: Mat4, pan: f32, tilt: f32) -> (Pose, Vec3) {
    let root = mvr_transform * gdtf_to_local();
    let mut world = vec![Mat4::IDENTITY; ty.parts.len()];
    for (i, part) in ty.parts.iter().enumerate() {
        let parent = part.parent.map_or(root, |p| world[p]);
        let mut m = parent * part.local;
        if Some(i) == ty.pan {
            m *= Mat4::from_rotation_z(pan.to_radians());
        }
        if Some(i) == ty.tilt {
            m *= Mat4::from_rotation_x(tilt.to_radians());
        }
        world[i] = m;
    }
    let rot = |m: Mat4| {
        let m = Mat3::from_mat4(m);
        Mat3::from_cols(m.x_axis.normalize(), m.y_axis.normalize(), m.z_axis.normalize())
    };
    // GDTF frames to the renderer's body convention (standing, beam +Y at tilt 0)
    let to_body = Mat3::from_rotation_x(-PI / 2.0);
    let beam_part = ty.beam.map_or(0, |b| b.part);
    let pan_part = ty.pan.unwrap_or(0);
    let pivot_part = ty.tilt.or(ty.pan).unwrap_or(beam_part);
    let pose = Pose {
        mount: rot(world[0]) * to_body,
        pan_frame: rot(world[pan_part]) * to_body,
        // beam space +Z = GDTF beam direction −Z
        beam: rot(world[beam_part]) * Mat3::from_rotation_x(PI),
        lens: world[beam_part].w_axis.truncate(),
    };
    (pose, world[pivot_part].w_axis.truncate())
}

const SCENE_ALBEDO: f32 = 0.35;

/// Triangles of all MVR scene meshes in world space, interleaved for the renderer.
/// Returns the vertex data and the files that could not be read.
pub fn scene_vertices(scene: &Scene) -> (Vec<f32>, Vec<String>) {
    let mut cache: HashMap<&str, Option<Vec<Vec3>>> = HashMap::new();
    let mut out = Vec::new();
    let mut failed = Vec::new();
    for m in &scene.meshes {
        let tris = cache.entry(m.file_name.as_str()).or_insert_with(|| match load_mesh(scene, m) {
            Ok(t) => Some(t),
            Err(e) => {
                failed.push(format!("{}: {e:#}", m.file_name));
                None
            }
        });
        let Some(tris) = tris else { continue };
        let mut mesh = Mesh { pos: Vec::with_capacity(tris.len()), normal: Vec::with_capacity(tris.len()) };
        for t in tris.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|k| m.transform.transform_point3(t[k]));
            let n = (b - a).cross(c - a).normalize_or_zero();
            mesh.pos.extend([a, b, c]);
            mesh.normal.extend([n; 3]);
        }
        interleave(&mesh, SCENE_ALBEDO, &mut out);
    }
    (out, failed)
}

/// Triangle list in MVR file units (Z-up, millimetres).
fn load_mesh(scene: &Scene, m: &MeshInstance) -> Result<Vec<Vec3>> {
    let data = scene.files.get(&m.file_name).ok_or_else(|| anyhow!("missing in MVR"))?;
    let lower = m.file_name.to_lowercase();
    if lower.ends_with(".3ds") {
        let parsed = ds3::parse(data).map_err(|e| anyhow!("{e}"))?;
        let mut tris = Vec::new();
        for mesh in &parsed.meshes {
            for f in &mesh.faces {
                tris.extend(f.iter().map(|&i| Vec3::from(mesh.vertices[i as usize])));
            }
        }
        Ok(tris)
    } else if lower.ends_with(".glb") || lower.ends_with(".gltf") {
        let (doc, buffers, _) = gltf::import_slice(data).context("glTF")?;
        let mut tris = Vec::new();
        // glTF is Y-up in metres
        let to_mvr = Mat4::from_scale(Vec3::splat(1000.0)) * Mat4::from_rotation_x(PI / 2.0);
        let scene_root = doc.default_scene().or_else(|| doc.scenes().next()).ok_or_else(|| anyhow!("no scene"))?;
        let mut stack: Vec<(gltf::Node, Mat4)> = scene_root.nodes().map(|n| (n, to_mvr)).collect();
        while let Some((node, parent)) = stack.pop() {
            let m = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
            if let Some(mesh) = node.mesh() {
                for prim in mesh.primitives() {
                    let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
                    let Some(positions) = reader.read_positions() else { continue };
                    let pos: Vec<Vec3> = positions.map(|p| m.transform_point3(Vec3::from(p))).collect();
                    match reader.read_indices() {
                        Some(idx) => tris.extend(idx.into_u32().map(|i| pos[i as usize])),
                        None => tris.extend(pos),
                    }
                }
            }
            stack.extend(node.children().map(|c| (c, m)));
        }
        Ok(tris)
    } else {
        Err(anyhow!("unsupported mesh format"))
    }
}

/// Axis-aligned bounds of the fixtures' pivots, for framing the camera.
pub fn bounds(fixtures: &[Fixture]) -> Option<(Vec3, Vec3)> {
    let first = fixtures.first()?.pos;
    Some(fixtures.iter().fold((first, first), |(lo, hi), f| (lo.min(f.pos), hi.max(f.pos))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_type::{Part, PartKind};

    /// Base → Yoke (pan) → Head (tilt) → Beam 0.3 m below the tilt axis, hanging as GDTF models it.
    fn mover() -> FixtureType {
        let part = |name: &str, parent: Option<usize>, kind, z: f32| Part {
            name: name.into(),
            parent,
            kind,
            local: Mat4::from_translation(Vec3::new(0.0, 0.0, z)),
            model: None,
        };
        FixtureType {
            manufacturer: String::new(),
            name: "Mover".into(),
            mode: "Std".into(),
            parts: vec![
                part("Base", None, PartKind::Generic, 0.0),
                part("Yoke", Some(0), PartKind::Axis, -0.2),
                part("Head", Some(1), PartKind::Axis, -0.3),
                part("Beam", Some(2), PartKind::Beam, -0.3),
            ],
            pan: Some(1),
            tilt: Some(2),
            beam: Some(Beam { part: 3, kind: BeamKind::Spot, beam_angle: 20.0, field_angle: 20.0, radius: 0.05, luminous_flux: 0.0 }),
            wheels: Vec::new(),
            models: Vec::new(),
            missing_media: 0,
        }
    }

    #[test]
    fn hanging_fixture_points_down_and_tilt_swings_beam() {
        let ty = mover();
        // MVR identity at 6 m height (Y-up world after io::mvr)
        let at = Mat4::from_translation(Vec3::new(0.0, 6.0, 0.0));
        let (p, pivot) = pose(&ty, at, 0.0, 0.0);
        assert!(pivot.abs_diff_eq(Vec3::new(0.0, 5.5, 0.0), 1e-5), "{pivot}");
        assert!(p.lens.abs_diff_eq(Vec3::new(0.0, 5.2, 0.0), 1e-5), "{}", p.lens);
        assert!(p.beam.z_axis.abs_diff_eq(Vec3::NEG_Y, 1e-5), "{}", p.beam.z_axis);
        // tilt 90° about the head's X axis lifts the beam to horizontal
        let (p, _) = pose(&ty, at, 0.0, 90.0);
        assert!(p.beam.z_axis.y.abs() < 1e-5 && (p.beam.z_axis.length() - 1.0).abs() < 1e-5);
        // pan turns that horizontal beam about the vertical
        let (q, _) = pose(&ty, at, 90.0, 90.0);
        assert!(q.beam.z_axis.y.abs() < 1e-5);
        assert!(p.beam.z_axis.dot(q.beam.z_axis).abs() < 1e-5);
        // the renderer's body frames agree with its own convention: hanging base = Rx(π)
        let (p, _) = pose(&ty, at, 0.0, 0.0);
        assert!(p.mount.y_axis.abs_diff_eq(Vec3::NEG_Y, 1e-5), "{}", p.mount.y_axis);
    }
}
