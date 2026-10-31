//! Fixture state (one entry per moving head) and conversion to GPU instance data.
//! The state is the single source of truth; Art-Net/DMX will later write into it.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use glam::{Mat3, Vec3};

pub const LENS_OFFSET: f32 = 0.14;
pub const BEAM_LENGTH: f32 = 40.0;
pub const BEAM_FLOATS: usize = 22; // apex 3, basis 9, cone 3 (tan half angle, length, focus), color 3, shadow origin 3 + slot 1
pub const BODY_FLOATS: usize = 33; // pos 3, mount 9, pan 9, beam 9, lens color 3
pub const LIGHT_FLOATS: usize = 7; // slot 1, wheel 1 (pos, rot, blur) 3, wheel 2 3
pub const WHEEL_PLANES: [f32; 2] = [0.0, 0.3]; // focus value at which each gobo wheel is sharp
pub const FOCUS_BLUR: f32 = 8.0; // mip levels of blur per unit of defocus

#[derive(Clone, Debug)]
pub struct Fixture {
    pub pos: Vec3, // head center (tilt axis), world
    pub hanging: bool,
    pub pan: f32,  // degrees
    pub tilt: f32, // degrees, 0 = beam points away from base
    pub dimmer: f32,
    pub color: Vec3, // linear RGB 0..1
    pub zoom: f32,   // full beam angle, degrees
    pub focus: f32,  // 0..1, selects the sharp plane (see WHEEL_PLANES)
    pub gobo: f32,   // wheel position: 0 open, n = gobo n-1, fraction = wheel in motion
    pub gobo_rot: f32,
    pub gobo2: f32,
    pub gobo2_rot: f32,
    pub prism: u32, // facet count, 0 = out
    pub prism_rot: f32,
    pub prism_spread: f32, // deflection per facet, degrees
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            pos: Vec3::ZERO,
            hanging: true,
            pan: 0.0,
            tilt: 0.0,
            dimmer: 1.0,
            color: Vec3::ONE,
            zoom: 14.0,
            focus: 0.0,
            gobo: 0.0,
            gobo_rot: 0.0,
            gobo2: 0.0,
            gobo2_rot: 0.0,
            prism: 0,
            prism_rot: 0.0,
            prism_spread: 7.0,
        }
    }
}

/// (mount, pan frame, beam basis); beam basis maps beam axis +Z into the world.
fn frames(f: &Fixture) -> (Mat3, Mat3, Mat3) {
    let mount = if f.hanging { Mat3::from_rotation_x(PI) } else { Mat3::IDENTITY };
    let pan_frame = mount * Mat3::from_rotation_y(f.pan.to_radians());
    let beam = pan_frame * Mat3::from_rotation_x(f.tilt.to_radians()) * Mat3::from_rotation_x(-FRAC_PI_2);
    (mount, pan_frame, beam)
}

/// Pan/tilt (degrees) that point the beam at a world position.
pub fn aim_at(f: &Fixture, target: Vec3) -> (f32, f32) {
    let d = (target - f.pos).normalize();
    let flip = if f.hanging { -1.0 } else { 1.0 };
    let (dx, dy, dz) = (d.x, d.y * flip, d.z * flip);
    ((-dx).atan2(-dz).to_degrees(), -dy.clamp(-1.0, 1.0).acos().to_degrees())
}

pub fn build_body_instances(fixtures: &[Fixture]) -> Vec<f32> {
    let mut out = Vec::with_capacity(fixtures.len() * BODY_FLOATS);
    for f in fixtures {
        let (mount, pan_frame, beam) = frames(f);
        out.extend(f.pos.to_array());
        out.extend(mount.to_cols_array());
        out.extend(pan_frame.to_cols_array());
        out.extend(beam.to_cols_array());
        out.extend((f.color * f.dimmer).to_array());
    }
    out
}

/// Per fixture: everything in front of the lens, composed into one light image tile.
pub fn build_light_instances(fixtures: &[Fixture]) -> Vec<f32> {
    let mut out = Vec::with_capacity(fixtures.len() * LIGHT_FLOATS);
    for (i, f) in fixtures.iter().enumerate() {
        out.extend([
            i as f32,
            f.gobo, f.gobo_rot.to_radians(), (f.focus - WHEEL_PLANES[0]).abs() * FOCUS_BLUR,
            f.gobo2, f.gobo2_rot.to_radians(), (f.focus - WHEEL_PLANES[1]).abs() * FOCUS_BLUR,
        ]);
    }
    out
}

/// One cone instance per beam; an active prism splits a fixture into one beam per facet.
pub fn build_beam_instances(fixtures: &[Fixture]) -> Vec<f32> {
    let mut out = Vec::new();
    for (i, f) in fixtures.iter().enumerate() {
        if f.dimmer <= 1e-3 {
            continue;
        }
        let (_, _, base) = frames(f);
        let facets = f.prism.max(1);
        let spread = if f.prism > 0 { f.prism_spread.to_radians() } else { 0.0 };
        let apex = f.pos + base.z_axis * LENS_OFFSET;
        let color = f.color * (f.dimmer / facets as f32);
        for k in 0..facets {
            let phi = f.prism_rot.to_radians() + TAU * k as f32 / facets as f32;
            let basis = base * Mat3::from_rotation_z(phi) * Mat3::from_rotation_y(spread) * Mat3::from_rotation_z(-phi);
            out.extend(apex.to_array());
            out.extend(basis.to_cols_array());
            out.extend([(f.zoom.to_radians() / 2.0).tan(), BEAM_LENGTH, f.focus]);
            out.extend(color.to_array());
            // slot = fixture index: selects the light image tile and the shadow maps (centered on the head)
            out.extend([f.pos.x, f.pos.y, f.pos.z, i as f32]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beam_axis(f: &Fixture) -> Vec3 {
        frames(f).2.z_axis
    }

    #[test]
    fn hanging_neutral_points_down() {
        let f = Fixture::default();
        assert!(beam_axis(&f).abs_diff_eq(Vec3::NEG_Y, 1e-6));
    }

    #[test]
    fn aim_at_points_the_beam_at_the_target() {
        for hanging in [true, false] {
            let mut f = Fixture { pos: Vec3::new(1.0, 5.0, -2.0), hanging, ..Default::default() };
            for target in [Vec3::new(0.0, 4.0, 12.0), Vec3::new(-6.0, 0.0, -7.0), Vec3::new(3.0, 1.0, 0.0)] {
                (f.pan, f.tilt) = aim_at(&f, target);
                assert!(beam_axis(&f).abs_diff_eq((target - f.pos).normalize(), 1e-4));
            }
        }
    }

    #[test]
    fn prism_splits_into_deflected_facets() {
        let f = Fixture { prism: 3, prism_spread: 7.0, ..Default::default() };
        let data = build_beam_instances(std::slice::from_ref(&f));
        assert_eq!(data.len(), 3 * BEAM_FLOATS);
        for beam in data.chunks(BEAM_FLOATS) {
            let az = Vec3::new(beam[9], beam[10], beam[11]);
            assert!((az.angle_between(beam_axis(&f)).to_degrees() - 7.0).abs() < 1e-3);
            assert!((beam[15] - 1.0 / 3.0).abs() < 1e-6);
        }
    }

    #[test]
    fn dark_fixtures_produce_no_beams() {
        let f = Fixture { dimmer: 0.0, ..Default::default() };
        assert!(build_beam_instances(&[f]).is_empty());
    }
}
