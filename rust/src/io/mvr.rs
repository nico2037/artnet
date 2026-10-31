//! MVR reader: rig, patch and scene meshes from `GeneralSceneDescription.xml`.
//!
//! MVR is Z-up in millimetres; everything returned here is already in the renderer's
//! world (Y-up, metres, see `to_world`).

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use glam::{Mat4, Vec3, Vec4};
use roxmltree::Node;

pub const SCENE_FILE: &str = "GeneralSceneDescription.xml";
pub const UNIVERSE_SIZE: u32 = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    /// GDTF DMXBreak this address belongs to (MVR `break`, 0 is the first).
    pub dmx_break: u32,
    /// 1-based, as in MVR.
    pub universe: u32,
    /// 1..=512
    pub channel: u32,
}

impl Address {
    /// MVR writes either an absolute address (1-based over all universes) or `universe.channel`.
    /// Absolute 0 marks an unpatched break (`None`).
    pub fn parse(text: &str, dmx_break: u32) -> Result<Option<Self>> {
        let text = text.trim();
        let (universe, channel) = match text.split_once('.') {
            Some((u, c)) => (u.trim().parse::<u32>()?, c.trim().parse::<u32>()?),
            None => match text.parse::<u32>()? {
                0 => return Ok(None),
                absolute => ((absolute - 1) / UNIVERSE_SIZE + 1, (absolute - 1) % UNIVERSE_SIZE + 1),
            },
        };
        if universe == 0 || !(1..=UNIVERSE_SIZE).contains(&channel) {
            bail!("DMX address {text} out of range");
        }
        Ok(Some(Self { dmx_break, universe, channel }))
    }
}

#[derive(Clone, Debug)]
pub struct Fixture {
    pub uuid: String,
    pub name: String,
    /// `FixtureID` as written (may be text); `FixtureIDNumeric` is preferred for sorting when present.
    pub fixture_id: String,
    pub fixture_id_numeric: Option<i64>,
    pub gdtf_spec: String,
    pub gdtf_mode: String,
    /// Empty for an unpatched fixture.
    pub addresses: Vec<Address>,
    pub invert_pan: bool,
    pub invert_tilt: bool,
    /// World transform of the fixture origin (Y-up, metres).
    pub transform: Mat4,
    pub layer: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    SceneObject,
    Truss,
    Support,
    VideoScreen,
    Projector,
    /// Geometry attached directly to a fixture in the MVR (the GDTF model is drawn separately).
    Fixture,
}

/// One mesh file placed in the world. Several instances usually share a file.
#[derive(Clone, Debug)]
pub struct MeshInstance {
    pub file_name: String,
    pub kind: ObjectKind,
    pub object_name: String,
    /// World transform of the file's coordinates (MVR geometry units, Z-up) into the renderer's world.
    /// 3DS files are in millimetres, glTF in metres; the loader applies the unit scale.
    pub transform: Mat4,
}

#[derive(Debug, Default)]
pub struct Scene {
    pub fixtures: Vec<Fixture>,
    pub meshes: Vec<MeshInstance>,
    /// GDTF and mesh files in the archive, by name.
    pub files: HashMap<String, Vec<u8>>,
}

impl Scene {
    pub fn load(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::read(file).with_context(|| format!("reading MVR {}", path.display()))
    }

    pub fn read<R: Read + Seek>(reader: R) -> Result<Self> {
        let mut zip = zip::ZipArchive::new(reader)?;
        let mut files = HashMap::new();
        let mut xml = None;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            if name == SCENE_FILE {
                xml = Some(String::from_utf8(data).context("scene description is no UTF-8")?);
            } else {
                files.insert(name, data);
            }
        }
        let xml = xml.ok_or_else(|| anyhow!("{SCENE_FILE} missing"))?;
        let mut scene = parse_scene(&xml)?;
        scene.files = files;
        Ok(scene)
    }

    /// GDTF archive of a fixture. Some exporters leave the `.gdtf` extension out of `GDTFSpec`.
    pub fn gdtf_bytes(&self, fixture: &Fixture) -> Option<&[u8]> {
        let spec = fixture.gdtf_spec.as_str();
        self.files.get(spec).or_else(|| self.files.get(&format!("{spec}.gdtf"))).map(Vec::as_slice)
    }

    /// MVR universes used by the patch (1-based).
    pub fn universes(&self) -> Vec<u32> {
        let mut out: Vec<u32> = self.fixtures.iter().flat_map(|f| f.addresses.iter().map(|a| a.universe)).collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// MVR `{u1,u2,u3}{v1,v2,v3}{w1,w2,w3}{o1,o2,o3}`: u, v, w are the basis columns, o the offset in mm.
pub fn parse_matrix(text: &str) -> Result<Mat4> {
    let mut cols = [Vec3::ZERO; 4];
    let mut groups = text.split('{').map(str::trim).filter(|g| !g.is_empty());
    for col in &mut cols {
        let group = groups.next().ok_or_else(|| anyhow!("matrix needs 4 groups: {text}"))?;
        let values: Vec<f32> = group
            .trim_end_matches('}')
            .split(',')
            .map(|v| v.trim().parse::<f32>())
            .collect::<Result<_, _>>()
            .with_context(|| format!("matrix {text}"))?;
        if values.len() != 3 {
            bail!("matrix group needs 3 values: {text}");
        }
        *col = Vec3::from_slice(&values);
    }
    if groups.next().is_some() {
        bail!("matrix has more than 4 groups: {text}");
    }
    Ok(Mat4::from_cols(cols[0].extend(0.0), cols[1].extend(0.0), cols[2].extend(0.0), cols[3].extend(1.0)))
}

/// Z-up millimetres to the renderer's Y-up metres: (x, y, z) → (x, z, −y) / 1000.
pub fn mvr_to_world() -> Mat4 {
    Mat4::from_cols(Vec4::X, Vec4::NEG_Z, Vec4::Y, Vec4::W) * Mat4::from_scale(Vec3::splat(0.001))
}

/// Transform of an object (accumulated MVR matrix) as a world transform for points in its local frame,
/// with local coordinates in metres and Y-up, the convention of the GDTF models after conversion.
pub fn to_world(mvr: Mat4) -> Mat4 {
    // local metres → local mm → MVR world mm → world metres; the local axis swap mirrors the world one
    mvr_to_world() * mvr * mvr_to_world().inverse()
}

fn parse_scene(xml: &str) -> Result<Scene> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    let scene_node = child(root, "Scene").ok_or_else(|| anyhow!("no <Scene>"))?;
    let mut symdefs = HashMap::new();
    if let Some(aux) = child(scene_node, "AUXData") {
        for symdef in aux.children().filter(|n| n.has_tag_name("Symdef")) {
            if let Some(uuid) = symdef.attribute("uuid") {
                symdefs.insert(uuid.to_string(), symdef);
            }
        }
    }
    let mut walker = Walker { symdefs, scene: Scene::default(), layer: String::new() };
    let layers = child(scene_node, "Layers").ok_or_else(|| anyhow!("no <Layers>"))?;
    for layer in layers.children().filter(|n| n.has_tag_name("Layer")) {
        walker.layer = layer.attribute("name").unwrap_or_default().to_string();
        let transform = local_matrix(layer)?;
        walker.child_list(layer, transform)?;
    }
    Ok(walker.scene)
}

struct Walker<'a, 'input> {
    symdefs: HashMap<String, Node<'a, 'input>>,
    scene: Scene,
    layer: String,
}

impl<'a, 'input> Walker<'a, 'input> {
    fn child_list(&mut self, parent: Node<'a, 'input>, transform: Mat4) -> Result<()> {
        let Some(list) = child(parent, "ChildList") else { return Ok(()) };
        for node in list.children().filter(Node::is_element) {
            self.object(node, transform)?;
        }
        Ok(())
    }

    fn object(&mut self, node: Node<'a, 'input>, parent: Mat4) -> Result<()> {
        let transform = parent * local_matrix(node)?;
        let name = node.attribute("name").unwrap_or_default().to_string();
        let kind = match node.tag_name().name() {
            "Fixture" => {
                self.scene.fixtures.push(fixture(node, &name, transform, &self.layer)?);
                ObjectKind::Fixture
            }
            "SceneObject" => ObjectKind::SceneObject,
            "Truss" => ObjectKind::Truss,
            "Support" => ObjectKind::Support,
            "VideoScreen" => ObjectKind::VideoScreen,
            "Projector" => ObjectKind::Projector,
            "GroupObject" => return self.child_list(node, transform),
            // FocusPoint and unknown elements carry no geometry
            _ => return Ok(()),
        };
        if let Some(geometries) = child(node, "Geometries") {
            self.geometries(geometries, transform, kind, &name, 0)?;
        }
        self.child_list(node, transform)
    }

    /// `Geometry3D` places a file, `Symbol` instantiates a `Symdef` (which may contain further symbols).
    fn geometries(&mut self, list: Node<'a, 'input>, transform: Mat4, kind: ObjectKind, name: &str, depth: u32) -> Result<()> {
        if depth > 16 {
            bail!("Symdef nesting deeper than 16 levels at {name}");
        }
        for g in list.children().filter(Node::is_element) {
            let local = transform * local_matrix(g)?;
            match g.tag_name().name() {
                "Geometry3D" => {
                    let file_name = g.attribute("fileName").ok_or_else(|| anyhow!("Geometry3D without fileName in {name}"))?;
                    self.scene.meshes.push(MeshInstance {
                        file_name: file_name.to_string(),
                        kind,
                        object_name: name.to_string(),
                        transform: mvr_to_world() * local,
                    });
                }
                "Symbol" => {
                    let uuid = g.attribute("symdef").unwrap_or_default();
                    let symdef = *self.symdefs.get(uuid).ok_or_else(|| anyhow!("unknown Symdef {uuid} in {name}"))?;
                    if let Some(list) = child(symdef, "ChildList") {
                        self.geometries(list, local, kind, name, depth + 1)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn fixture(node: Node, name: &str, transform: Mat4, layer: &str) -> Result<Fixture> {
    let text = |tag: &str| child(node, tag).and_then(|n| n.text()).map(str::trim).unwrap_or_default().to_string();
    let flag = |tag: &str| matches!(text(tag).to_lowercase().as_str(), "true" | "1");
    let mut addresses = Vec::new();
    if let Some(list) = child(node, "Addresses") {
        for a in list.children().filter(|n| n.has_tag_name("Address")) {
            let dmx_break = a.attribute("break").unwrap_or("0").trim().parse()?;
            addresses.extend(Address::parse(a.text().unwrap_or_default(), dmx_break).with_context(|| format!("fixture {name}"))?);
        }
    }
    Ok(Fixture {
        uuid: node.attribute("uuid").unwrap_or_default().to_string(),
        name: name.to_string(),
        fixture_id: text("FixtureID"),
        fixture_id_numeric: text("FixtureIDNumeric").parse().ok(),
        gdtf_spec: text("GDTFSpec"),
        gdtf_mode: text("GDTFMode"),
        addresses,
        invert_pan: flag("DMXInvertPan"),
        invert_tilt: flag("DMXInvertTilt"),
        transform: to_world(transform),
        layer: layer.to_string(),
    })
}

fn child<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|n| n.has_tag_name(tag))
}

fn local_matrix(node: Node) -> Result<Mat4> {
    match child(node, "Matrix").and_then(|m| m.text()) {
        Some(text) if !text.trim().is_empty() => parse_matrix(text),
        _ => Ok(Mat4::IDENTITY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_absolute_and_universe_dot_channel() {
        let parse = |t| Address::parse(t, 0).unwrap().unwrap();
        assert_eq!(parse("1"), Address { dmx_break: 0, universe: 1, channel: 1 });
        assert_eq!(parse("512").universe, 1);
        assert_eq!(Address::parse("513", 1).unwrap(), Some(Address { dmx_break: 1, universe: 2, channel: 1 }));
        assert_eq!(parse(" 3.17 "), Address { dmx_break: 0, universe: 3, channel: 17 });
        assert_eq!(Address::parse("0", 0).unwrap(), None);
        assert!(Address::parse("x", 0).is_err());
        assert!(Address::parse("1.513", 0).is_err());
        assert!(Address::parse("0.1", 0).is_err());
    }

    #[test]
    fn matrix_columns_are_u_v_w_offset() {
        let m = parse_matrix("{0,1,0}{-1,0,0}{0,0,1}{100,200,300}").unwrap();
        assert_eq!(m.transform_point3(Vec3::ZERO), Vec3::new(100.0, 200.0, 300.0));
        // u is the image of local x
        assert_eq!(m.transform_vector3(Vec3::X), Vec3::Y);
        assert!(parse_matrix("{1,0,0}{0,1,0}{0,0,1}").is_err());
        assert!(parse_matrix("{1,0}{0,1,0}{0,0,1}{0,0,0}").is_err());
    }

    #[test]
    fn z_up_millimetres_become_y_up_metres() {
        let w = mvr_to_world();
        assert!(w.transform_point3(Vec3::new(1000.0, 2000.0, 3000.0)).abs_diff_eq(Vec3::new(1.0, 3.0, -2.0), 1e-6));
        // an object 5 m up and 2 m downstage, rotated 90° about MVR Z
        let mvr = parse_matrix("{0,1,0}{-1,0,0}{0,0,1}{0,-2000,5000}").unwrap();
        let world = to_world(mvr);
        assert!(world.transform_point3(Vec3::ZERO).abs_diff_eq(Vec3::new(0.0, 5.0, 2.0), 1e-5));
        // MVR Z rotation is a rotation about world Y; local metres stay metres
        assert!(world.transform_vector3(Vec3::X).abs_diff_eq(Vec3::new(0.0, 0.0, -1.0), 1e-6));
        assert!(world.transform_vector3(Vec3::Y).abs_diff_eq(Vec3::Y, 1e-6));
    }

    const SCENE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<GeneralSceneDescription verMajor="1" verMinor="6"><Scene>
  <AUXData>
    <Symdef uuid="S1" name="pipe"><ChildList>
      <Geometry3D fileName="pipe.3ds"><Matrix>{1,0,0}{0,1,0}{0,0,1}{0,0,10}</Matrix></Geometry3D>
    </ChildList></Symdef>
  </AUXData>
  <Layers>
    <Layer uuid="L1" name="Rig"><Matrix>{1,0,0}{0,1,0}{0,0,1}{1000,0,0}</Matrix><ChildList>
      <GroupObject uuid="G" name="Grp"><Matrix>{1,0,0}{0,1,0}{0,0,1}{0,0,6000}</Matrix><ChildList>
        <Fixture uuid="F1" name="Spot 1">
          <Matrix>{1,0,0}{0,1,0}{0,0,1}{500,0,0}</Matrix>
          <GDTFSpec>Maker@Spot.gdtf</GDTFSpec><GDTFMode>Standard</GDTFMode>
          <FixtureID>101</FixtureID><FixtureIDNumeric>101</FixtureIDNumeric>
          <Addresses><Address break="0">2.1</Address><Address break="1">1025</Address></Addresses>
          <DMXInvertPan>true</DMXInvertPan>
        </Fixture>
        <Truss uuid="T1" name="Truss A"><Geometries>
          <Symbol uuid="X" symdef="S1"><Matrix>{1,0,0}{0,1,0}{0,0,1}{0,0,5}</Matrix></Symbol>
          <Geometry3D fileName="box.glb"/>
        </Geometries></Truss>
      </ChildList></GroupObject>
      <FocusPoint uuid="P" name="FP"/>
    </ChildList></Layer>
  </Layers>
</Scene></GeneralSceneDescription>"#;

    #[test]
    fn scene_walks_layers_groups_symbols() {
        let scene = parse_scene(SCENE).unwrap();
        assert_eq!(scene.fixtures.len(), 1);
        let f = &scene.fixtures[0];
        assert_eq!((f.name.as_str(), f.fixture_id_numeric, f.layer.as_str()), ("Spot 1", Some(101), "Rig"));
        assert_eq!((f.gdtf_spec.as_str(), f.gdtf_mode.as_str()), ("Maker@Spot.gdtf", "Standard"));
        assert_eq!(f.addresses[0], Address { dmx_break: 0, universe: 2, channel: 1 });
        assert_eq!(f.addresses[1], Address { dmx_break: 1, universe: 3, channel: 1 });
        assert!(f.invert_pan && !f.invert_tilt);
        // layer x 1 m + fixture x 0.5 m, group 6 m up
        assert!(f.transform.transform_point3(Vec3::ZERO).abs_diff_eq(Vec3::new(1.5, 6.0, 0.0), 1e-5));

        assert_eq!(scene.meshes.len(), 2);
        let pipe = &scene.meshes[0];
        assert_eq!((pipe.file_name.as_str(), pipe.kind, pipe.object_name.as_str()), ("pipe.3ds", ObjectKind::Truss, "Truss A"));
        // file units stay mm: origin lands 1 m along x, 6 m + 15 mm up
        assert!(pipe.transform.transform_point3(Vec3::ZERO).abs_diff_eq(Vec3::new(1.0, 6.015, 0.0), 1e-5));
        assert_eq!(scene.meshes[1].file_name, "box.glb");
        assert_eq!(scene.universes(), vec![2, 3]);
    }

    #[test]
    fn gdtf_spec_resolves_with_or_without_extension() {
        let mut scene = parse_scene(SCENE).unwrap();
        scene.files.insert("Maker@Spot.gdtf".into(), vec![1]);
        assert_eq!(scene.gdtf_bytes(&scene.fixtures[0]), Some(&[1u8][..]));
        scene.fixtures[0].gdtf_spec = "Maker@Spot".into();
        assert_eq!(scene.gdtf_bytes(&scene.fixtures[0]), Some(&[1u8][..]));
        scene.fixtures[0].gdtf_spec = "Other".into();
        assert_eq!(scene.gdtf_bytes(&scene.fixtures[0]), None);
    }

    #[test]
    fn unknown_symdef_is_an_error() {
        let xml = SCENE.replace(r#"symdef="S1""#, r#"symdef="nope""#);
        assert!(parse_scene(&xml).unwrap_err().to_string().contains("nope"));
    }
}
