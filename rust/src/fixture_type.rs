//! Render-side view of a GDTF fixture type in one DMX mode: geometry chain, pan/tilt axes,
//! beam, wheels (colours, gobos, prism facets) and models.
//!
//! Geometry stays in GDTF units and axes (metres, Z-up, hanging, beam along −Z); `io::mvr`
//! places the fixture in the world.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;

use anyhow::{anyhow, Result};
use gdtf::geometry::{AnyGeometry, BeamType, Geometry};
use gdtf::values::{ColorCie, Matrix, Name, Rotation};
use gdtf::wheel::WheelSlotOptic;
use gdtf::GdtfFile;
use glam::{Mat3, Mat4, Vec3};

use crate::resources::{gobo_transmission, GOBO_SIZE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    Generic,
    Axis,
    Beam,
    Filter,
    /// GeometryReference, media server, laser, wiring, … (not drawn, children not expanded)
    Other,
}

#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub parent: Option<usize>,
    pub kind: PartKind,
    /// Relative to the parent, GDTF axes and metres.
    pub local: Mat4,
    pub model: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeamKind {
    /// Hard edge, gobos in focus.
    Spot,
    /// PC / Fresnel: soft edge.
    Wash,
    /// Glow, None, Rectangle: emits light but no defined beam.
    Glow,
}

#[derive(Clone, Copy, Debug)]
pub struct Beam {
    pub part: usize,
    pub kind: BeamKind,
    /// Full angles in degrees.
    pub beam_angle: f32,
    pub field_angle: f32,
    /// Lens radius in metres.
    pub radius: f32,
    pub luminous_flux: f32,
}

#[derive(Clone, Debug)]
pub struct Facet {
    pub color: Vec3,
    /// 2D affine transform of the facet in beam space (GDTF `Rotation`, row-major as written).
    pub transform: Mat3,
}

#[derive(Clone, Debug)]
pub struct Slot {
    pub name: String,
    /// Linear RGB transmission, 1 = clear.
    pub color: Vec3,
    pub media: Option<String>,
    /// Layer in the `GoboLibrary`; `None` when the slot has no image or the archive lacks it.
    pub gobo: Option<u32>,
    pub facets: Vec<Facet>,
}

#[derive(Clone, Debug)]
pub struct Wheel {
    pub name: String,
    /// GDTF slot index n is `slots[n - 1]`.
    pub slots: Vec<Slot>,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub name: String,
    /// File name in `models/3ds|gltf/` without extension; `None` = primitive only.
    pub file: Option<String>,
    /// Length (x), width (y), height (z) in metres; the mesh is scaled to this box.
    pub size: Vec3,
}

#[derive(Clone, Debug)]
pub struct FixtureType {
    pub manufacturer: String,
    pub name: String,
    pub mode: String,
    /// Parent before child; `parts[0]` is the root geometry of the mode.
    pub parts: Vec<Part>,
    pub pan: Option<usize>,
    pub tilt: Option<usize>,
    pub beam: Option<Beam>,
    pub wheels: Vec<Wheel>,
    pub models: Vec<Model>,
    /// Wheel slots whose image is referenced but missing from the archive.
    pub missing_media: usize,
}

/// Wheel images of all fixture types as GOBO_SIZE² R8 layers, identical images stored once.
#[derive(Default)]
pub struct GoboLibrary {
    pub layers: Vec<Vec<u8>>,
    index: HashMap<u64, u32>,
}

impl GoboLibrary {
    pub fn add(&mut self, image: Vec<u8>) -> u32 {
        debug_assert_eq!(image.len(), GOBO_SIZE * GOBO_SIZE);
        let mut hasher = DefaultHasher::new();
        image.hash(&mut hasher);
        let key = hasher.finish();
        if let Some(&layer) = self.index.get(&key) {
            if self.layers[layer as usize] == image {
                return layer;
            }
        }
        let layer = self.layers.len() as u32;
        self.layers.push(image);
        self.index.insert(key, layer);
        layer
    }
}

/// CIE xyY (Y 0..100) to linear sRGB (D65), clamped to 0..1.
pub fn cie_to_rgb(c: &ColorCie) -> Vec3 {
    if c.y <= 0.0 {
        return Vec3::ZERO;
    }
    let big_y = c.z / 100.0;
    let x = c.x / c.y * big_y;
    let z = (1.0 - c.x - c.y) / c.y * big_y;
    let rgb = Vec3::new(
        (3.2406 * x - 1.5372 * big_y - 0.4986 * z) as f32,
        (-0.9689 * x + 1.8758 * big_y + 0.0415 * z) as f32,
        (0.0557 * x - 0.2040 * big_y + 1.0570 * z) as f32,
    );
    rgb.clamp(Vec3::ZERO, Vec3::ONE)
}

/// gdtf keeps matrices private; their serialised form `{a,b,c,d}{…}` is the way out.
fn rows<T: serde::Serialize>(value: &T) -> Result<Vec<Vec<f32>>> {
    let text: String = serde_json::from_value(serde_json::to_value(value)?)?;
    text.split('{')
        .map(str::trim)
        .filter(|g| !g.is_empty())
        .map(|g| g.trim_end_matches('}').split(',').map(|v| v.trim().parse::<f32>().map_err(Into::into)).collect())
        .collect()
}

/// GDTF 4×4 matrix, row-major with the translation in the 4th column.
fn matrix(m: &Matrix) -> Result<Mat4> {
    let r = rows(m)?;
    if r.len() != 4 || r.iter().any(|row| row.len() != 4) {
        return Err(anyhow!("GDTF matrix is not 4×4: {r:?}"));
    }
    Ok(Mat4::from_cols_array_2d(&[
        [r[0][0], r[1][0], r[2][0], r[3][0]],
        [r[0][1], r[1][1], r[2][1], r[3][1]],
        [r[0][2], r[1][2], r[2][2], r[3][2]],
        [r[0][3], r[1][3], r[2][3], r[3][3]],
    ]))
}

/// GDTF 3×3 rotation, row-major.
fn rotation(m: &Rotation) -> Result<Mat3> {
    let r = rows(m)?;
    if r.len() != 3 || r.iter().any(|row| row.len() != 3) {
        return Err(anyhow!("GDTF rotation is not 3×3: {r:?}"));
    }
    Ok(Mat3::from_cols_array_2d(&[[r[0][0], r[1][0], r[2][0]], [r[0][1], r[1][1], r[2][1]], [r[0][2], r[1][2], r[2][2]]]))
}

fn position(g: &Geometry) -> &Matrix {
    match g {
        Geometry::Generic(g) => &g.position,
        Geometry::Axis(g) => &g.position,
        Geometry::FilterBeam(g) => &g.position,
        Geometry::FilterColor(g) => &g.position,
        Geometry::FilterGobo(g) => &g.position,
        Geometry::FilterShaper(g) => &g.position,
        Geometry::Beam(g) => &g.position,
        Geometry::MediaServerLayer(g) => &g.position,
        Geometry::MediaServerCamera(g) => &g.position,
        Geometry::MediaServerMaster(g) => &g.position,
        Geometry::Display(g) => &g.position,
        Geometry::Reference(g) => &g.position,
        Geometry::Laser(g) => &g.position,
        Geometry::WiringObject(g) => &g.position,
        Geometry::Inventory(g) => &g.position,
        Geometry::Structure(g) => &g.position,
        Geometry::Support(g) => &g.position,
        Geometry::Magnet(g) => &g.position,
    }
}

fn name_of(name: Option<&Name>) -> String {
    name.map(|n| n.to_string()).unwrap_or_default()
}

impl FixtureType {
    /// The fixture type in `mode`; wheel images go into `gobos`.
    pub fn from_gdtf(file: &mut GdtfFile, mode: &str, gobos: &mut GoboLibrary) -> Result<Self> {
        let ft = file.description.fixture_types.first().ok_or_else(|| anyhow!("GDTF without fixture type"))?;
        // the gdtf crate stores names with characters outside GDTF Annex C replaced by `_`, the MVR keeps them
        let dmx_mode = ft.dmx_mode(&Name::new_lossy(mode)).ok_or_else(|| {
            let modes: Vec<String> = ft.dmx_modes.iter().map(|m| name_of(m.name.as_ref())).collect();
            anyhow!("{} {} has no mode \"{mode}\" (modes: {modes:?})", ft.manufacturer, ft.long_name)
        })?;
        let models: Vec<Model> = ft
            .models
            .iter()
            .map(|m| Model {
                name: name_of(m.name.as_ref()),
                file: m.file.clone().filter(|f| !f.is_empty()),
                size: Vec3::new(m.length as f32, m.width as f32, m.height as f32),
            })
            .collect();
        let root_name = name_of(dmx_mode.geometry.as_ref());
        let root = ft.root_geometry(&root_name).ok_or_else(|| anyhow!("mode {mode}: no geometry \"{root_name}\""))?;
        let mut parts = Vec::new();
        let mut beam = None;
        flatten(root, None, &models, &mut parts, &mut beam)?;

        // the DMX channel of the Pan/Tilt attribute names the axis it moves
        let axis = |attribute: &str| {
            dmx_mode
                .dmx_channels
                .iter()
                .find(|c| c.logical_channels.iter().any(|l| l.attribute.first().is_some_and(|a| a.as_ref() == attribute)))
                .and_then(|c| parts.iter().position(|p| p.name == c.geometry.as_ref()))
        };
        let (pan, tilt) = (axis("Pan"), axis("Tilt"));

        let mut missing_media = 0;
        let mut wheels = Vec::new();
        for wheel in &ft.wheels {
            let mut slots = Vec::new();
            for slot in &wheel.slots {
                let color = match &slot.optic {
                    WheelSlotOptic::Color(c) => cie_to_rgb(c),
                    // filter references need the emitter spectrum; treat as clear for now
                    WheelSlotOptic::Filter(_) => Vec3::ONE,
                };
                let media = slot.media_name.clone().filter(|m| !m.is_empty());
                let gobo = match &media {
                    Some(m) => match file.resources.read_wheel_media(m) {
                        Ok(mut r) => {
                            let mut png = Vec::new();
                            r.read_to_end(&mut png)?;
                            Some(gobos.add(gobo_transmission(&png)?))
                        }
                        Err(_) => {
                            missing_media += 1;
                            None
                        }
                    },
                    None => None,
                };
                let facets = slot
                    .facets
                    .iter()
                    .map(|f| Ok(Facet { color: cie_to_rgb(&f.color), transform: rotation(&f.rotation)? }))
                    .collect::<Result<_>>()?;
                slots.push(Slot { name: name_of(slot.name.as_ref()), color, media, gobo, facets });
            }
            wheels.push(Wheel { name: name_of(wheel.name.as_ref()), slots });
        }

        Ok(Self {
            manufacturer: ft.manufacturer.clone(),
            name: ft.long_name.clone(),
            mode: mode.to_string(),
            parts,
            pan,
            tilt,
            beam,
            wheels,
            models,
            missing_media,
        })
    }

    pub fn wheel(&self, name: &str) -> Option<&Wheel> {
        self.wheels.iter().find(|w| w.name == name)
    }

    /// Transform of a part relative to the fixture origin with all axes at rest.
    pub fn rest_transform(&self, part: usize) -> Mat4 {
        let mut m = self.parts[part].local;
        let mut parent = self.parts[part].parent;
        while let Some(p) = parent {
            m = self.parts[p].local * m;
            parent = self.parts[p].parent;
        }
        m
    }
}

fn flatten(g: &Geometry, parent: Option<usize>, models: &[Model], parts: &mut Vec<Part>, beam: &mut Option<Beam>) -> Result<()> {
    let kind = match g {
        Geometry::Generic(_) => PartKind::Generic,
        Geometry::Axis(_) => PartKind::Axis,
        Geometry::Beam(_) => PartKind::Beam,
        Geometry::FilterBeam(_) | Geometry::FilterColor(_) | Geometry::FilterGobo(_) | Geometry::FilterShaper(_) => PartKind::Filter,
        _ => PartKind::Other,
    };
    let index = parts.len();
    let model_name = g.model_name().map(|n| n.to_string());
    parts.push(Part {
        name: name_of(g.name()),
        parent,
        kind,
        local: matrix(position(g))?,
        model: model_name.and_then(|n| models.iter().position(|m| m.name == n)),
    });
    if let (Geometry::Beam(b), None) = (g, &beam) {
        *beam = Some(Beam {
            part: index,
            kind: match b.beam_type {
                BeamType::Spot => BeamKind::Spot,
                BeamType::Pc | BeamType::Fresnel => BeamKind::Wash,
                _ => BeamKind::Glow,
            },
            beam_angle: b.beam_angle as f32,
            field_angle: b.field_angle as f32,
            radius: b.beam_radius as f32,
            luminous_flux: b.luminous_flux as f32,
        });
    }
    if kind != PartKind::Other {
        for child in g.children() {
            flatten(child, Some(index), models, parts, beam)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cie(x: f64, y: f64, big_y: f64) -> ColorCie {
        ColorCie { x, y, z: big_y }
    }

    #[test]
    fn cie_white_red_and_dark() {
        assert!(cie_to_rgb(&cie(0.3127, 0.3290, 100.0)).abs_diff_eq(Vec3::ONE, 0.01));
        let red = cie_to_rgb(&cie(0.64, 0.33, 21.26));
        assert!(red.abs_diff_eq(Vec3::new(1.0, 0.0, 0.0), 0.01), "{red}");
        assert_eq!(cie_to_rgb(&cie(0.3, 0.0, 50.0)), Vec3::ZERO);
        // a half-transmitting neutral filter
        assert!(cie_to_rgb(&cie(0.3127, 0.3290, 50.0)).abs_diff_eq(Vec3::splat(0.5), 0.01));
    }

    #[test]
    fn gdtf_matrix_translation_is_fourth_column() {
        let m: Matrix = serde_json::from_value("{1,0,0,0.1}{0,0,-1,0.2}{0,1,0,0.3}{0,0,0,1}".into()).unwrap();
        let m = matrix(&m).unwrap();
        assert_eq!(m.transform_point3(Vec3::ZERO), Vec3::new(0.1, 0.2, 0.3));
        // rows: y' = -z, z' = y
        assert_eq!(m.transform_vector3(Vec3::Z), Vec3::new(0.0, -1.0, 0.0));
        assert_eq!(m.transform_vector3(Vec3::Y), Vec3::new(0.0, 0.0, 1.0));
    }

    #[test]
    fn gobo_library_dedupes_identical_images() {
        let mut lib = GoboLibrary::default();
        let a = vec![1u8; GOBO_SIZE * GOBO_SIZE];
        let b = vec![2u8; GOBO_SIZE * GOBO_SIZE];
        assert_eq!(lib.add(a.clone()), 0);
        assert_eq!(lib.add(b), 1);
        assert_eq!(lib.add(a), 0);
        assert_eq!(lib.layers.len(), 2);
    }
}
