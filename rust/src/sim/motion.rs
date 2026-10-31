//! Motion model: the console sets targets, the fixture follows with limited speed and acceleration.
//!
//! Pan/tilt: `v_max = range / RealFade`, `a_max = v_max / RealAcceleration`, per frame
//! `v_want = sign(err) · min(v_max, sqrt(2 · a_max · |err|))` (discrete form), `v` approaches `v_want` by at most
//! `a_max · dt`. Wheels travel slot by slot the short way round, spin and rotate; continuous
//! attributes are low-pass filtered so 30–44 Hz DMX steps do not show.

use glam::Vec3;

use crate::dmx::resolve::{Shutter, Target, Timing, WheelTarget};
use crate::fixture_type::FixtureType;

/// Fallbacks when the GDTF leaves RealFade/RealAcceleration empty (typical spot).
pub const PAN_DEFAULT: Timing = Timing { fade: 2.5, acceleration: 0.3 };
pub const TILT_DEFAULT: Timing = Timing { fade: 1.8, acceleration: 0.3 };
/// Used when the GDTF gives no pan/tilt range.
pub const PAN_RANGE_DEFAULT: f32 = 540.0;
pub const TILT_RANGE_DEFAULT: f32 = 270.0;
/// Seconds to move the wheel by one slot.
pub const WHEEL_SECONDS_PER_SLOT: f32 = 0.35;
/// Indexing speed of a gobo/prism, degrees per second.
pub const INDEX_SPEED: f32 = 360.0;
/// Time constant smoothing DMX steps of dimmer, colour, zoom, focus, iris, frost.
pub const SMOOTHING: f32 = 0.03;
/// Fraction of a strobe period the shutter is open.
pub const STROBE_DUTY: f32 = 0.25;

/// One motor axis with speed and acceleration limits.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Axis {
    pub pos: f32,
    pub vel: f32,
}

impl Axis {
    pub fn step(&mut self, target: f32, v_max: f32, a_max: f32, dt: f32) {
        let err = target - self.pos;
        if err == 0.0 && self.vel == 0.0 {
            return;
        }
        // fastest speed from which the axis can still brake before the target; the dt term makes the
        // discrete braking land on the target instead of creeping up on it
        let half = 0.5 * a_max * dt;
        let v_brake = -half + (half * half + 2.0 * a_max * err.abs()).sqrt();
        let v_want = err.signum() * v_max.min(v_brake);
        self.vel += (v_want - self.vel).clamp(-a_max * dt, a_max * dt);
        let next = self.pos + self.vel * dt;
        // arriving: this step reaches or passes the target
        if (target - next).signum() != err.signum() || (target - next).abs() < 1e-4 {
            self.pos = target;
            self.vel = 0.0;
        } else {
            self.pos = next;
        }
    }
}

fn limits(range: f32, timing: Timing, default: Timing) -> (f32, f32) {
    let fade = if timing.fade > 0.0 { timing.fade } else { default.fade };
    let acceleration = if timing.acceleration > 0.0 { timing.acceleration } else { default.acceleration };
    let v_max = range / fade;
    (v_max, v_max / acceleration)
}

/// First-order low-pass towards `target`.
fn smooth(value: &mut f32, target: f32, dt: f32) {
    *value += (target - *value) * (1.0 - (-dt / SMOOTHING).exp());
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelState {
    pub wheel: Option<usize>,
    pub number: u8,
    /// 1-based slot position; fractional while the wheel travels or spins.
    pub position: f32,
    /// Orientation of the slot (gobo/prism rotation), degrees.
    pub angle: f32,
}

impl WheelState {
    fn new(target: &WheelTarget) -> Self {
        Self { wheel: target.wheel, number: target.number, position: target.slot.max(1) as f32, angle: target.index }
    }

    fn step(&mut self, target: &WheelTarget, slots: usize, dt: f32) {
        let slots = slots.max(1) as f32;
        if target.spin != 0.0 {
            // WheelSpin: degrees per second of the whole wheel
            self.position += target.spin / 360.0 * slots * dt;
        } else if target.slot > 0 {
            // shortest way round; positions run 1..=slots
            let mut diff = (target.slot as f32 - self.position).rem_euclid(slots);
            if diff > slots / 2.0 {
                diff -= slots;
            }
            let step = dt / WHEEL_SECONDS_PER_SLOT;
            self.position += diff.clamp(-step, step);
        }
        self.position = (self.position - 1.0).rem_euclid(slots) + 1.0;
        match target.rotation {
            Some(speed) => self.angle = (self.angle + speed * dt).rem_euclid(360.0),
            None => {
                let mut diff = (target.index - self.angle).rem_euclid(360.0);
                if diff > 180.0 {
                    diff -= 360.0;
                }
                let step = INDEX_SPEED * dt;
                self.angle = (self.angle + diff.clamp(-step, step)).rem_euclid(360.0);
            }
        }
    }
}

/// What the fixture shows right now.
#[derive(Clone, Debug, PartialEq)]
pub struct Actual {
    pub pan: f32,
    pub tilt: f32,
    /// Dimmer × shutter/strobe, 0..1.
    pub intensity: f32,
    pub color: Vec3,
    pub color_wheels: Vec<WheelState>,
    pub gobos: Vec<WheelState>,
    pub prisms: Vec<WheelState>,
    pub zoom: Option<f32>,
    pub focus: f32,
    pub iris: f32,
    pub frost: f32,
}

/// Per-fixture motion state between frames.
#[derive(Clone, Debug)]
pub struct Motion {
    pub pan: Axis,
    pub tilt: Axis,
    dimmer: f32,
    color: Vec3,
    zoom: Option<f32>,
    focus: f32,
    iris: f32,
    frost: f32,
    strobe_phase: f32,
    strobe_cycle: u32,
    wheels: [Vec<WheelState>; 3],
}

impl Motion {
    /// Starts at rest at the target (fixture already in position when the rig loads).
    pub fn new(target: &Target) -> Self {
        let init = |list: &[WheelTarget]| list.iter().map(WheelState::new).collect();
        Self {
            pan: Axis { pos: target.pan, vel: 0.0 },
            tilt: Axis { pos: target.tilt, vel: 0.0 },
            dimmer: target.dimmer,
            color: target.color,
            zoom: target.zoom,
            focus: target.focus,
            iris: target.iris,
            frost: target.frost,
            strobe_phase: 0.0,
            strobe_cycle: 0,
            wheels: [init(&target.color_wheels), init(&target.gobos), init(&target.prisms)],
        }
    }

    pub fn step(&mut self, target: &Target, ft: &FixtureType, dt: f32) -> Actual {
        let pan_range = if target.pan_range > 0.0 { target.pan_range } else { PAN_RANGE_DEFAULT };
        let tilt_range = if target.tilt_range > 0.0 { target.tilt_range } else { TILT_RANGE_DEFAULT };
        let (v, a) = limits(pan_range, target.pan_timing, PAN_DEFAULT);
        self.pan.step(target.pan, v, a, dt);
        let (v, a) = limits(tilt_range, target.tilt_timing, TILT_DEFAULT);
        self.tilt.step(target.tilt, v, a, dt);

        smooth(&mut self.dimmer, target.dimmer, dt);
        for k in 0..3 {
            smooth(&mut self.color[k], target.color[k], dt);
        }
        self.zoom = match (self.zoom, target.zoom) {
            (Some(mut z), Some(t)) => {
                smooth(&mut z, t, dt);
                Some(z)
            }
            (_, t) => t,
        };
        smooth(&mut self.focus, target.focus, dt);
        smooth(&mut self.iris, target.iris, dt);
        smooth(&mut self.frost, target.frost, dt);

        let gate = match target.shutter {
            Shutter::Open => 1.0,
            Shutter::Closed => 0.0,
            Shutter::Strobe { hz, random } => self.strobe(hz.max(0.0), random, dt),
        };

        let targets = [&target.color_wheels, &target.gobos, &target.prisms];
        for (states, targets) in self.wheels.iter_mut().zip(targets) {
            for t in targets.iter() {
                let slots = t.wheel.and_then(|w| ft.wheels.get(w)).map_or(1, |w| w.slots.len());
                match states.iter_mut().find(|s| s.number == t.number) {
                    Some(s) => s.step(t, slots, dt),
                    None => states.push(WheelState::new(t)),
                }
            }
        }

        Actual {
            pan: self.pan.pos,
            tilt: self.tilt.pos,
            intensity: (self.dimmer * gate).clamp(0.0, 1.0),
            color: self.color,
            color_wheels: self.wheels[0].clone(),
            gobos: self.wheels[1].clone(),
            prisms: self.wheels[2].clone(),
            zoom: self.zoom,
            focus: self.focus,
            iris: self.iris,
            frost: self.frost,
        }
    }

    /// Strobe gate: open for STROBE_DUTY of each period; random strobe varies each period's length.
    fn strobe(&mut self, hz: f32, random: bool, dt: f32) -> f32 {
        if hz <= 0.0 {
            return 1.0;
        }
        let speed = if random {
            // 0.5..1.5 × rate, fixed for one period
            let h = self.strobe_cycle.wrapping_mul(2_654_435_761) >> 16;
            hz * (0.5 + (h & 0xffff) as f32 / 65535.0)
        } else {
            hz
        };
        self.strobe_phase += speed * dt;
        if self.strobe_phase >= 1.0 {
            self.strobe_cycle = self.strobe_cycle.wrapping_add(self.strobe_phase as u32);
            self.strobe_phase = self.strobe_phase.fract();
        }
        (self.strobe_phase < STROBE_DUTY) as u8 as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 120.0;

    fn run(axis: &mut Axis, target: f32, v_max: f32, a_max: f32, seconds: f32) -> (f32, f32) {
        let mut peak = 0.0f32;
        for _ in 0..(seconds / DT) as usize {
            axis.step(target, v_max, a_max, DT);
            peak = peak.max(axis.vel.abs());
        }
        (axis.pos, peak)
    }

    #[test]
    fn axis_reaches_target_without_overshoot_within_speed_limit() {
        let (v_max, a_max) = limits(540.0, Timing::default(), PAN_DEFAULT);
        assert_eq!((v_max, a_max), (216.0, 720.0));
        let mut axis = Axis::default();
        let mut max_pos = 0.0f32;
        let mut time_to_arrive = None;
        for i in 0..(5.0 / DT) as usize {
            axis.step(270.0, v_max, a_max, DT);
            max_pos = max_pos.max(axis.pos);
            assert!(axis.vel.abs() <= v_max + 1e-3);
            if axis.pos == 270.0 && time_to_arrive.is_none() {
                time_to_arrive = Some(i as f32 * DT);
            }
        }
        assert_eq!(max_pos, 270.0);
        // 270° at 216°/s plus ramp up/down (0.3 s each, half the distance) ≈ 1.55 s
        let t = time_to_arrive.unwrap();
        assert!((1.5..1.7).contains(&t), "{t}");
    }

    #[test]
    fn short_moves_never_reach_full_speed_and_reverse_cleanly() {
        let mut axis = Axis::default();
        let (_, peak) = run(&mut axis, 5.0, 216.0, 720.0, 1.0);
        assert_eq!(axis.pos, 5.0);
        // triangle profile: sqrt(a · d) = 60°/s
        assert!(peak < 65.0, "{peak}");
        // reverse while moving
        axis.step(100.0, 216.0, 720.0, 0.2);
        let (pos, _) = run(&mut axis, -50.0, 216.0, 720.0, 2.0);
        assert_eq!(pos, -50.0);
        assert_eq!(axis.vel, 0.0);
    }

    #[test]
    fn real_fade_from_gdtf_wins_over_defaults() {
        let (v, a) = limits(540.0, Timing { fade: 3.134, acceleration: 0.0 }, PAN_DEFAULT);
        assert!((v - 172.3).abs() < 0.1);
        assert!((a - v / 0.3).abs() < 1e-3);
    }

    fn wheel_target(slot: u32) -> WheelTarget {
        WheelTarget { wheel: None, number: 1, slot, spin: 0.0, index: 0.0, rotation: None }
    }

    #[test]
    fn wheel_takes_short_way_round_one_slot_per_step_time() {
        let mut w = WheelState::new(&wheel_target(1));
        // 8 slots: 1 → 7 goes backwards through 8 (two slots)
        let target = wheel_target(7);
        let mut t = 0.0;
        while w.position != 7.0 && t < 3.0 {
            w.step(&target, 8, DT);
            assert!(w.position >= 7.0 - 1e-4 || w.position <= 1.0 + 1e-4, "went the long way: {}", w.position);
            t += DT;
        }
        assert!((t - 2.0 * WHEEL_SECONDS_PER_SLOT).abs() < 0.02, "{t}");
    }

    #[test]
    fn spin_and_rotation_integrate() {
        let mut w = WheelState::new(&wheel_target(1));
        let spin = WheelTarget { spin: 90.0, rotation: Some(-45.0), ..wheel_target(1) };
        for _ in 0..120 {
            w.step(&spin, 8, DT);
        }
        // a quarter turn of an 8-slot wheel = 2 slots; slot rotation −45°/s
        assert!((w.position - 3.0).abs() < 1e-3, "{}", w.position);
        assert!((w.angle - 315.0).abs() < 1e-2, "{}", w.angle);
        // indexing takes the short way and stops at the target
        let index = WheelTarget { index: 10.0, ..wheel_target(1) };
        for _ in 0..120 {
            w.step(&index, 8, DT);
        }
        assert!((w.angle - 10.0).abs() < 1e-2, "{}", w.angle);
    }

    #[test]
    fn strobe_gate_follows_rate_and_duty() {
        let ft_target = Target { dimmer: 1.0, shutter: Shutter::Strobe { hz: 10.0, random: false }, ..Target::default() };
        let mut m = Motion::new(&ft_target);
        let (mut on, mut flashes, mut was_on) = (0, 0, false);
        for _ in 0..1200 {
            let is_on = m.strobe(10.0, false, DT) > 0.0;
            on += is_on as u32;
            flashes += (is_on && !was_on) as u32;
            was_on = is_on;
        }
        // 10 s at 10 Hz
        assert!((99..=101).contains(&flashes), "{flashes}");
        assert!(((on as f32 / 1200.0) - STROBE_DUTY).abs() < 0.02);
    }

    #[test]
    fn dmx_steps_are_smoothed() {
        let mut d = 0.0;
        smooth(&mut d, 1.0, 0.03);
        assert!((d - (1.0 - (-1.0f32).exp())).abs() < 1e-6);
    }
}
