//! Demo scene, rig, automatic timeline and manual keyboard control.

use std::collections::HashSet;
use std::f32::consts::TAU;

use glam::{vec3, Vec3};
use winit::keyboard::KeyCode;

use crate::fixtures::{aim_at, Fixture};
use crate::meshes::{cuboid, interleave, quad};

pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const EYE: Vec3 = vec3(0.0, 4.0, 12.0);
pub const TARGET: Vec3 = vec3(0.0, 2.2, -3.0);
pub const FOV: f32 = 50.0;
const TRUSS_Y: f32 = 6.2; // truss underside
const PER_ROW: usize = 25;
const LOOP: f32 = 38.0; // seconds
const BLINDER: (f32, f32) = (29.0, 37.0); // all beams aim into the camera
pub const PAN_SPEED: f32 = 60.0; // degrees per second while an arrow key is held
const ROTATE_STEP: f32 = 15.0; // rotation speed change per key press, degrees per second
const ZOOM_SPEED: f32 = 15.0;
const FOCUS_SPEED: f32 = 0.3;
const WHEEL_SECONDS_PER_SLOT: f32 = 0.35;

pub const MANUAL_HELP: &str = "Manual mode (all fixtures together):
  Arrows  pan / tilt           G  next gobo slot (wheel 1)
  Q / E   gobo rotation slower / faster (per press, continuous)
  A / D   prism rotation slower / faster (per press, continuous)
  H       next gobo slot (wheel 2)     P  prism in / out
  W / S   zoom wider / narrower
  R / F   focus (wheel 1 sharp at 0.0, wheel 2 at 0.3)
  C       next color";

const PALETTE: [Vec3; 6] = [
    vec3(1.0, 1.0, 1.0), vec3(1.0, 0.25, 0.1), vec3(0.2, 0.5, 1.0),
    vec3(1.0, 0.8, 0.3), vec3(0.3, 1.0, 0.5), vec3(0.9, 0.3, 1.0),
];

/// Interleaved (pos, normal, albedo) for floor, back wall, trusses and pillars.
pub fn build_stage(truss_rows: &[f32]) -> Vec<f32> {
    let mut out = Vec::new();
    let floor = [vec3(-15.0, 0.0, -8.0), vec3(15.0, 0.0, -8.0), vec3(15.0, 0.0, 14.0), vec3(-15.0, 0.0, 14.0)];
    interleave(&quad(floor, Vec3::Y), 0.3, &mut out);
    let wall = [vec3(-15.0, 0.0, -8.0), vec3(15.0, 0.0, -8.0), vec3(15.0, 12.0, -8.0), vec3(-15.0, 12.0, -8.0)];
    interleave(&quad(wall, Vec3::Z), 0.4, &mut out);
    for &z in truss_rows {
        interleave(&cuboid(vec3(-7.0, TRUSS_Y, z - 0.15), vec3(7.0, TRUSS_Y + 0.3, z + 0.15)), 0.35, &mut out);
    }
    for x in [-3.0, 0.0, 3.0] {
        interleave(&cuboid(vec3(x - 0.2, 0.0, -4.7), vec3(x + 0.2, 3.2, -4.3)), 0.35, &mut out);
    }
    out
}

/// Fixtures hanging under trusses (25 per truss); returns fixtures and truss z positions.
pub fn create_rig(count: usize) -> (Vec<Fixture>, Vec<f32>) {
    let rows = count.div_ceil(PER_ROW);
    let row_z: Vec<f32> = (0..rows).map(|r| -2.0 + (r as f32 - (rows as f32 - 1.0) / 2.0) * 1.4).collect();
    let fixtures = (0..count)
        .map(|i| {
            let (r, c) = (i / PER_ROW, i % PER_ROW);
            let in_row = PER_ROW.min(count - r * PER_ROW);
            let x = if in_row == 1 { 0.0 } else { -6.0 + 12.0 * c as f32 / (in_row - 1) as f32 };
            Fixture { pos: vec3(x, TRUSS_Y - 0.52, row_z[r]), ..Default::default() }
        })
        .collect();
    (fixtures, row_z)
}

fn window(t: f32, start: f32, end: f32, fade: f32) -> f32 {
    let w = ((t - start) / fade).clamp(0.0, 1.0).min(((end - t) / fade).clamp(0.0, 1.0));
    w * w * (3.0 - 2.0 * w)
}

/// schedule: [(time, slot)], first at 0; the wheel travels forward slot by slot.
fn wheel_position(tl: f32, schedule: &[(f32, f32)], slots: f32) -> f32 {
    let k = schedule.iter().rposition(|&(t, _)| t <= tl).unwrap_or(0);
    let start = schedule[(k + schedule.len() - 1) % schedule.len()].1;
    let (t_change, target) = schedule[k];
    let delta = (target - start).rem_euclid(slots);
    let travel = ((tl - t_change) / (WHEEL_SECONDS_PER_SLOT * delta.max(1.0))).clamp(0.0, 1.0);
    start + delta * travel * travel * (3.0 - 2.0 * travel)
}

pub fn animate(fixtures: &mut [Fixture], t: f32, gobo_count: usize) {
    // every fixture gets its own state (fixture 0 = reference demo) so no beam shares work with another
    let tl = t.rem_euclid(LOOP);
    let blind = window(tl, BLINDER.0, BLINDER.1, 2.5);
    let blinder_phase = tl >= BLINDER.0 - 1.0;
    let slots = (gobo_count + 1) as f32;
    let zoom = 14.0 + 10.0 * window(tl, 20.0, 26.0, 1.5);
    let focus = 0.05 + 0.25 * window(tl, 9.5, 12.5, 1.0);
    for (i, f) in fixtures.iter_mut().enumerate() {
        let tp = t + 0.37 * i as f32;
        // fanned sweep across the audience at eye height, so beams pass the camera one after another
        let phase = TAU * (tl - BLINDER.0) / 6.0 + 0.4 * (i / PER_ROW) as f32;
        let (blind_pan, blind_tilt) = aim_at(f, vec3(2.0 * f.pos.x + 8.0 * phase.sin(), EYE.y, EYE.z));
        let sweep_pan = 40.0 * (TAU * tp / 16.0).sin();
        let sweep_tilt = 5.0 + 30.0 * (TAU * tp / 13.0).sin();
        f.pan = sweep_pan + (blind_pan - sweep_pan) * blind;
        f.tilt = sweep_tilt + (blind_tilt - sweep_tilt) * blind;

        let slot = |n: usize| if gobo_count > 0 { (1 + (n + i) % gobo_count) as f32 } else { 0.0 };
        f.gobo = wheel_position(tl, &[(0.0, 0.0), (6.0, slot(0)), (20.0, slot(1)), (BLINDER.0 - 1.0, 0.0)], slots);
        let direction = if i % 2 == 1 { -1.0 } else { 1.0 };
        f.gobo_rot = direction * (70.0 + 15.0 * (i % 5) as f32) * t;
        // wheel 2 joins for a focus morph: wheel 1 sharp -> wheel 2 sharp -> back
        f.gobo2 = wheel_position(tl, &[(0.0, 0.0), (8.5, slot(3)), (13.0, 0.0)], slots);
        f.gobo2_rot = 0.0;
        let prism_on = (13.0..28.0).contains(&tl) != (i % 2 == 1);
        f.prism = if prism_on && !blinder_phase { 3 } else { 0 };
        f.prism_rot = -45.0 * (1.0 + 0.3 * (i % 3) as f32) * t;
        f.zoom = zoom + (4.0 - zoom) * blind;
        f.focus = focus;
        f.color = PALETTE[i % PALETTE.len()];
    }
}

fn step_wheel(pos: f32, target: f32, slots: f32, dt: f32) -> f32 {
    let mut remaining = (target - pos).rem_euclid(slots);
    if remaining > slots - 1e-3 {
        remaining = 0.0; // float noise right at the target, not a full turn
    }
    (pos + remaining.min(dt / WHEEL_SECONDS_PER_SLOT)).rem_euclid(slots)
}

/// Static look driven by keys; pan/tilt come from the arrow-key offsets in the main loop.
pub struct ManualControl {
    slots: f32,
    gobo: f32,
    gobo2: f32,
    gobo_target: f32,
    gobo2_target: f32,
    gobo_rot: f32,
    prism_rot: f32,
    gobo_speed: f32,
    prism_speed: f32,
    prism: bool,
    zoom: f32,
    focus: f32,
    color: usize,
}

impl ManualControl {
    pub fn new(gobo_count: usize) -> Self {
        Self {
            slots: (gobo_count + 1) as f32,
            gobo: 0.0,
            gobo2: 0.0,
            gobo_target: 0.0,
            gobo2_target: 0.0,
            gobo_rot: 0.0,
            prism_rot: 0.0,
            gobo_speed: 0.0,
            prism_speed: 0.0,
            prism: false,
            zoom: 14.0,
            focus: 0.05,
            color: 0,
        }
    }

    pub fn key_down(&mut self, key: KeyCode) {
        match key {
            KeyCode::KeyG => self.gobo_target = (self.gobo_target + 1.0) % self.slots,
            KeyCode::KeyH => self.gobo2_target = (self.gobo2_target + 1.0) % self.slots,
            KeyCode::KeyP => self.prism = !self.prism,
            KeyCode::KeyC => self.color = (self.color + 1) % PALETTE.len(),
            KeyCode::KeyQ => self.gobo_speed -= ROTATE_STEP,
            KeyCode::KeyE => self.gobo_speed += ROTATE_STEP,
            KeyCode::KeyA => self.prism_speed -= ROTATE_STEP,
            KeyCode::KeyD => self.prism_speed += ROTATE_STEP,
            _ => {}
        }
    }

    pub fn update(&mut self, keys: &HashSet<KeyCode>, dt: f32) {
        let axis = |plus, minus| keys.contains(&plus) as i32 as f32 - keys.contains(&minus) as i32 as f32;
        self.gobo_rot += self.gobo_speed * dt;
        self.prism_rot += self.prism_speed * dt;
        self.zoom = (self.zoom + ZOOM_SPEED * dt * axis(KeyCode::KeyW, KeyCode::KeyS)).clamp(2.0, 60.0);
        self.focus = (self.focus + FOCUS_SPEED * dt * axis(KeyCode::KeyR, KeyCode::KeyF)).clamp(0.0, 1.0);
        self.gobo = step_wheel(self.gobo, self.gobo_target, self.slots, dt);
        self.gobo2 = step_wheel(self.gobo2, self.gobo2_target, self.slots, dt);
    }

    pub fn apply(&self, fixtures: &mut [Fixture]) {
        for f in fixtures {
            f.pan = 0.0;
            f.tilt = 0.0;
            f.gobo = self.gobo;
            f.gobo_rot = self.gobo_rot;
            f.gobo2 = self.gobo2;
            f.gobo2_rot = 0.0;
            f.prism = if self.prism { 3 } else { 0 };
            f.prism_rot = self.prism_rot;
            f.zoom = self.zoom;
            f.focus = self.focus;
            f.color = PALETTE[self.color];
        }
    }

    pub fn status(&self) -> String {
        format!(
            "gobo {}/{} {:+.0}°/s | prism {} {:+.0}°/s | zoom {:.0}° | focus {:.2}",
            self.gobo_target, self.gobo2_target, self.gobo_speed,
            if self.prism { "in" } else { "out" }, self.prism_speed, self.zoom, self.focus
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_travels_forward_and_wraps() {
        let schedule = [(0.0, 0.0), (6.0, 1.0), (20.0, 2.0), (28.0, 0.0)];
        assert_eq!(wheel_position(3.0, &schedule, 5.0), 0.0);
        assert_eq!(wheel_position(10.0, &schedule, 5.0), 1.0);
        let mid = wheel_position(28.5, &schedule, 5.0); // 2 -> 0 forward over slots 3 and 4
        assert!(mid > 2.0 && mid < 5.0);
        assert_eq!(wheel_position(37.0, &schedule, 5.0), 5.0); // == slot 0 modulo 5
    }

    #[test]
    fn manual_wheel_reaches_target_without_full_turn() {
        let mut pos = 0.0;
        for _ in 0..120 {
            pos = step_wheel(pos, 2.0, 5.0, 1.0 / 60.0);
        }
        assert!((pos - 2.0).abs() < 1e-4);
    }
}
