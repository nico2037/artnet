//! Orbit camera: rotates around a target point, pans in the view plane, zooms along the view ray.

use glam::{Mat4, Vec3};

/// Fixed views, looking at the current target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// From the audience (+Z), slightly above.
    Foh,
    Top,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    /// Degrees about +Y; 0 looks along −Z (camera on the +Z side).
    pub yaw: f32,
    /// Degrees above the horizon.
    pub pitch: f32,
    /// Vertical field of view in degrees.
    pub fov: f32,
}

const MIN_DISTANCE: f32 = 0.3;
const MAX_DISTANCE: f32 = 150.0;
const PITCH_LIMIT: f32 = 89.0;
/// Degrees per pixel of mouse travel.
const ORBIT_SPEED: f32 = 0.3;
/// Fraction of the distance per wheel notch.
const ZOOM_STEP: f32 = 0.12;

impl OrbitCamera {
    /// Camera at `eye` looking at `target`.
    pub fn look_at(eye: Vec3, target: Vec3, fov: f32) -> Self {
        let d = eye - target;
        let distance = d.length().max(MIN_DISTANCE);
        let pitch = (d.y / distance).clamp(-1.0, 1.0).asin().to_degrees();
        let yaw = d.x.atan2(d.z).to_degrees();
        Self { target, distance, yaw, pitch, fov }
    }

    pub fn eye(&self) -> Vec3 {
        let (yaw, pitch) = (self.yaw.to_radians(), self.pitch.to_radians());
        self.target + self.distance * Vec3::new(yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos())
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Y)
    }

    /// Mouse drag in pixels: right turns the camera around the target to the right.
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * ORBIT_SPEED;
        self.pitch = (self.pitch + dy * ORBIT_SPEED).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Mouse drag in pixels; the point under the cursor stays under it at the target's depth.
    pub fn pan(&mut self, dx: f32, dy: f32, viewport_height: f32) {
        let per_pixel = 2.0 * self.distance * (self.fov.to_radians() / 2.0).tan() / viewport_height.max(1.0);
        let forward = (self.target - self.eye()).normalize();
        let right = forward.cross(Vec3::Y).normalize_or(Vec3::X);
        let up = right.cross(forward);
        self.target += (-dx * right + dy * up) * per_pixel;
    }

    /// Wheel notches; positive moves closer.
    pub fn zoom(&mut self, notches: f32) {
        self.distance = (self.distance * (1.0 - ZOOM_STEP).powf(notches)).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    pub fn set_view(&mut self, view: View) {
        (self.yaw, self.pitch) = match view {
            View::Foh => (0.0, 15.0),
            View::Top => (0.0, PITCH_LIMIT),
            View::Left => (-90.0, 10.0),
            View::Right => (90.0, 10.0),
        };
    }

    /// Frames the box [lo, hi] (plus the floor below it) from the current direction.
    pub fn fit(&mut self, lo: Vec3, hi: Vec3) {
        let lo = lo.min(Vec3::new(lo.x, 0.0, lo.z));
        let center = (lo + hi) * 0.5;
        let radius = ((hi - lo).length() * 0.5).max(2.0);
        self.target = center;
        self.distance = (radius / (self.fov.to_radians() / 2.0).sin() * 1.25).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_at_roundtrip() {
        let (eye, target) = (Vec3::new(3.0, 4.0, 12.0), Vec3::new(0.0, 2.0, -3.0));
        let c = OrbitCamera::look_at(eye, target, 50.0);
        assert!(c.eye().abs_diff_eq(eye, 1e-4), "{}", c.eye());
        let from = Mat4::look_at_rh(eye, target, Vec3::Y);
        assert!(c.view().abs_diff_eq(from, 1e-4));
    }

    #[test]
    fn orbit_keeps_distance_and_clamps_pitch() {
        let mut c = OrbitCamera::look_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, 50.0);
        c.orbit(300.0, 1000.0);
        assert!((c.eye().length() - 10.0).abs() < 1e-4);
        assert_eq!(c.pitch, PITCH_LIMIT);
        // dragging right moves the camera to the left of where it looked from
        let mut c = OrbitCamera::look_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, 50.0);
        c.orbit(10.0, 0.0);
        assert!(c.eye().x < 0.0);
    }

    #[test]
    fn pan_moves_target_in_view_plane() {
        let mut c = OrbitCamera::look_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, 90.0);
        // 90° fov, distance 10: the viewport spans 20 m, so 360 px of 720 is 10 m
        c.pan(-360.0, 0.0, 720.0);
        assert!(c.target.abs_diff_eq(Vec3::new(10.0, 0.0, 0.0), 1e-3), "{}", c.target);
        c.pan(0.0, 360.0, 720.0);
        assert!(c.target.abs_diff_eq(Vec3::new(10.0, 10.0, 0.0), 1e-3), "{}", c.target);
    }

    #[test]
    fn zoom_and_fit() {
        let mut c = OrbitCamera::look_at(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO, 50.0);
        c.zoom(1.0);
        assert!((c.distance - 8.8).abs() < 1e-4);
        c.zoom(-1000.0);
        assert_eq!(c.distance, MAX_DISTANCE);
        c.fit(Vec3::new(-5.0, 5.0, -1.0), Vec3::new(5.0, 6.0, 1.0));
        assert!(c.target.abs_diff_eq(Vec3::new(0.0, 3.0, 0.0), 1e-4), "{}", c.target);
        // the bounding sphere fits the vertical field of view
        let radius = (Vec3::new(10.0, 6.0, 2.0)).length() * 0.5;
        assert!(c.distance * (25.0f32).to_radians().sin() >= radius);
    }
}
