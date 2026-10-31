//! Library spike (PLAN Phase 0, step 2): do `gdtf`, `ds3` and `gltf` read everything in `testdata/`?
//! Raw `gdtf` only reports (it rejects files without `DMXChannel/@Highlight`); through
//! `io::gdtf::load` every file must parse, and every MVR must load with all GDTFs it references.
//!
//! `cargo test --test testdata_spike -- --nocapture` prints a report per file.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use gdtf::{GdtfFile, Model3Detail, Model3Format};
use vol_light::io::{gdtf as gdtf_io, mvr};

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../testdata")
}

/// Files in `testdata/` and, when `tools/fetch_testdata.py` has run, `testdata/external/`.
fn files(ext: &str) -> Vec<PathBuf> {
    let external = std::fs::read_dir(testdata().join("external")).into_iter().flatten();
    let mut out: Vec<PathBuf> = std::fs::read_dir(testdata())
        .expect("testdata/ missing")
        .chain(external)
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)))
        .collect();
    out.sort();
    out
}

/// All GDTF archives: loose files plus those embedded in the MVRs.
fn gdtf_archives() -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> =
        files("gdtf").into_iter().map(|p| (p.display().to_string(), std::fs::read(&p).unwrap())).collect();
    for mvr in files("mvr") {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&mvr).unwrap()).unwrap();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            if entry.name().to_lowercase().ends_with(".gdtf") {
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                out.push((format!("{} / {}", mvr.file_name().unwrap().to_string_lossy(), entry.name()), bytes));
            }
        }
    }
    out
}

#[test]
fn gdtf_crate_report() {
    let archives = gdtf_archives();
    assert!(archives.len() >= 7, "expected the GDTFs from testdata/, got {}", archives.len());
    let mut failed = Vec::new();
    for (name, bytes) in archives {
        if let Err(e) = GdtfFile::new(Cursor::new(bytes.clone())) {
            let msg = format!("{e:?}");
            // the serde error is long; its tail names the missing field or bad value
            println!("{name}: raw gdtf crate fails: {}", &msg[msg.len().saturating_sub(70)..]);
            failed.push(name.clone());
        }
        let mut file = gdtf_io::load(bytes).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let types = &file.description.fixture_types;
        assert_eq!(types.len(), 1, "{name}");
        let ft = &types[0];
        let channels: Vec<usize> = ft.dmx_modes.iter().map(|m| m.dmx_channels.len()).collect();
        let functions: usize = ft
            .dmx_modes
            .iter()
            .flat_map(|m| &m.dmx_channels)
            .flat_map(|c| &c.logical_channels)
            .map(|l| l.channel_functions.len())
            .sum();
        let (mut media, mut media_ok, mut facets) = (0, 0, 0);
        for wheel in &ft.wheels {
            for slot in &wheel.slots {
                facets += slot.facets.len();
                if let Some(m) = &slot.media_name {
                    if m.is_empty() {
                        continue;
                    }
                    media += 1;
                    if let Ok(mut r) = file.resources.read_wheel_media(m) {
                        let mut png = Vec::new();
                        r.read_to_end(&mut png).unwrap();
                        media_ok += png.starts_with(b"\x89PNG") as usize;
                    }
                }
            }
        }
        let (mut models, mut meshes_ok, mut meshes_failed) = (0, 0, Vec::new());
        for model in &ft.models {
            let Some(model_file) = model.file.as_deref().filter(|f| !f.is_empty()) else { continue };
            models += 1;
            let mut loaded = false;
            for format in [Model3Format::Gltf, Model3Format::Max3ds] {
                let Ok(mut r) = file.resources.read_model_mesh(model_file, format, Model3Detail::Default) else {
                    continue;
                };
                let mut data = Vec::new();
                r.read_to_end(&mut data).unwrap();
                loaded = match format {
                    Model3Format::Gltf => gltf::Gltf::from_slice(&data).is_ok(),
                    Model3Format::Max3ds => ds3::parse(&data).is_ok_and(|s| !s.meshes.is_empty()),
                };
                break;
            }
            if loaded {
                meshes_ok += 1;
            } else {
                meshes_failed.push(model_file.to_string());
            }
        }
        println!(
            "{name}: {} {} | modes {channels:?}, {functions} channel functions | wheels {}, media {media_ok}/{media}, facets {facets} | models {meshes_ok}/{models}",
            ft.manufacturer,
            ft.long_name,
            ft.wheels.len(),
        );
        // MVR exporters often strip wheel images; the renderer falls back to a plain slot
        if media_ok != media {
            println!("  {} wheel images missing in the archive", media - media_ok);
        }
        assert!(meshes_failed.is_empty(), "{name}: models {meshes_failed:?}");
    }
    println!("raw gdtf crate: {} files rejected, all parsed through io::gdtf::load", failed.len());
}

#[test]
fn mvr_reader_loads_rigs_with_their_gdtfs() {
    for path in files("mvr") {
        let scene = mvr::Scene::load(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(!scene.fixtures.is_empty(), "{name}");
        for f in &scene.fixtures {
            let bytes = scene.gdtf_bytes(f).unwrap_or_else(|| panic!("{name}: {} references missing {}", f.name, f.gdtf_spec));
            gdtf_io::load(bytes.to_vec()).unwrap();
            let p = f.transform.w_axis;
            // stages fit in a 100 m box; catches mm/m mix-ups
            assert!(p.x.abs() < 100.0 && p.y.abs() < 100.0 && p.z.abs() < 100.0, "{name}: {} at {p}", f.name);
        }
        for m in &scene.meshes {
            assert!(scene.files.contains_key(&m.file_name), "{name}: missing mesh {}", m.file_name);
        }
        let heights: Vec<f32> = scene.fixtures.iter().map(|f| f.transform.w_axis.y).collect();
        println!(
            "{name}: {} fixtures, {} mesh instances, universes {:?}, fixture height {:.2}..{:.2} m",
            scene.fixtures.len(),
            scene.meshes.len(),
            scene.universes(),
            heights.iter().cloned().fold(f32::INFINITY, f32::min),
            heights.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        );
    }
}

#[test]
fn mvr_scene_xml_and_embedded_meshes_load() {
    for mvr in files("mvr") {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&mvr).unwrap()).unwrap();
        let mut xml = String::new();
        zip.by_name("GeneralSceneDescription.xml").unwrap().read_to_string(&mut xml).unwrap();
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let count = |tag: &str| doc.descendants().filter(|n| n.has_tag_name(tag)).count();
        let (mut ds, mut ds_ok, mut glb, mut glb_ok) = (0, 0, 0, 0);
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            let lower = entry.name().to_lowercase();
            let mut data = Vec::new();
            if lower.ends_with(".3ds") {
                entry.read_to_end(&mut data).unwrap();
                ds += 1;
                ds_ok += ds3::parse(&data).is_ok_and(|s| !s.meshes.is_empty()) as usize;
            } else if lower.ends_with(".glb") {
                entry.read_to_end(&mut data).unwrap();
                glb += 1;
                glb_ok += gltf::Gltf::from_slice(&data).is_ok() as usize;
            }
        }
        println!(
            "{}: {} fixtures, {} scene objects, {} trusses, {} Geometry3D, {} Symdef | 3ds {ds_ok}/{ds}, glb {glb_ok}/{glb}",
            mvr.file_name().unwrap().to_string_lossy(),
            count("Fixture"),
            count("SceneObject"),
            count("Truss"),
            count("Geometry3D"),
            count("Symdef"),
        );
        assert!(count("Fixture") > 0);
        assert_eq!((ds_ok, glb_ok), (ds, glb));
    }
}

#[test]
fn fixture_types_from_mvr_patch() {
    use vol_light::fixture_type::{FixtureType, GoboLibrary};
    let mut gobos = GoboLibrary::default();
    for path in files("mvr") {
        let scene = mvr::Scene::load(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy();
        let mut seen = std::collections::BTreeSet::new();
        for f in &scene.fixtures {
            if !seen.insert((f.gdtf_spec.clone(), f.gdtf_mode.clone())) {
                continue;
            }
            let mut file = gdtf_io::load(scene.gdtf_bytes(f).unwrap().to_vec()).unwrap();
            let ft = FixtureType::from_gdtf(&mut file, &f.gdtf_mode, &mut gobos)
                .unwrap_or_else(|e| panic!("{name}: {} mode {:?}: {e:?}", f.name, f.gdtf_mode));
            let Some(beam) = ft.beam else {
                // Capture exports carry placeholder GDTFs: one geometry, dummy channels
                println!("{name}: {} / {} | placeholder, {} parts, no beam", ft.name, ft.mode, ft.parts.len());
                continue;
            };
            let beam_rest = ft.rest_transform(beam.part);
            let slots: Vec<usize> = ft.wheels.iter().map(|w| w.slots.len()).collect();
            let facets: usize = ft.wheels.iter().flat_map(|w| &w.slots).map(|s| s.facets.len()).sum();
            println!(
                "{name}: {} / {} | {} parts, pan {:?} tilt {:?} | beam {:?} {:.1}°/{:.1}° r {:.3} m at {:.3} dir {:.2} | wheels {slots:?}, facets {facets}, missing media {}",
                ft.name,
                ft.mode,
                ft.parts.len(),
                ft.pan.map(|p| &ft.parts[p].name),
                ft.tilt.map(|p| &ft.parts[p].name),
                beam.kind,
                beam.beam_angle,
                beam.field_angle,
                beam.radius,
                beam_rest.w_axis.truncate(),
                beam_rest.transform_vector3(glam::Vec3::NEG_Z).normalize(),
                ft.missing_media,
            );
            // tilt hangs below pan in every moving head
            if let (Some(pan), Some(tilt)) = (ft.pan, ft.tilt) {
                let mut p = ft.parts[tilt].parent;
                while p.is_some() && p != Some(pan) {
                    p = ft.parts[p.unwrap()].parent;
                }
                assert_eq!(p, Some(pan), "{name}: {} tilt is not below pan", ft.name);
            }
        }
    }
    println!("gobo library: {} images", gobos.layers.len());
}

#[test]
fn loose_gdtfs_build_with_gobo_images() {
    use vol_light::fixture_type::{FixtureType, GoboLibrary};
    let mut gobos = GoboLibrary::default();
    for path in files("gdtf") {
        let mut file = gdtf_io::load(std::fs::read(&path).unwrap()).unwrap();
        let mode = file.description.fixture_types[0].dmx_modes[0].name.as_ref().unwrap().to_string();
        let ft = FixtureType::from_gdtf(&mut file, &mode, &mut gobos).unwrap();
        let images = ft.wheels.iter().flat_map(|w| &w.slots).filter(|s| s.gobo.is_some()).count();
        println!("{}: {} / {mode} | pan {:?} tilt {:?} | {images} slots with images", path.display(), ft.name, ft.pan, ft.tilt);
        assert_eq!(ft.missing_media, 0);
    }
    for layer in &gobos.layers {
        // a gobo has an opening and a blocked part
        let (min, max) = layer.iter().fold((255u8, 0u8), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        assert!(min < 32 && max > 200, "flat gobo image {min}..{max}");
    }
    println!("gobo library: {} distinct images", gobos.layers.len());
}

#[test]
fn megapointe_dmx_resolves_like_its_manual() {
    use vol_light::dmx::resolve::{Profile, Shutter};
    use vol_light::fixture_type::{FixtureType, GoboLibrary};
    let Some(path) = files("mvr").into_iter().find(|p| p.ends_with("basic_gdtf.mvr")) else {
        println!("basic_gdtf.mvr not fetched, skipped");
        return;
    };
    let scene = mvr::Scene::load(&path).unwrap();
    let fixture = &scene.fixtures[0];
    let mut file = gdtf_io::load(scene.gdtf_bytes(fixture).unwrap().to_vec()).unwrap();
    let mode = "Mode 1 - Standard 16 - bit";
    let ft = FixtureType::from_gdtf(&mut file, mode, &mut GoboLibrary::default()).unwrap();
    let profile = Profile::from_gdtf(&file, mode, &ft).unwrap();
    assert_eq!(profile.channels.iter().map(|c| c.offsets.len()).sum::<usize>(), 39);

    let at = [mvr::Address { dmx_break: 0, universe: 1, channel: 1 }];
    let run = |values: &[(usize, u8)]| {
        let mut data = [0u8; 512];
        for &(ch, v) in values {
            data[ch - 1] = v;
        }
        profile.resolve(&profile.read(&at, |u| (u == 1).then_some(&data)), false, false)
    };
    let wheel_name = |w: Option<usize>| w.map(|w| ft.wheels[w].name.as_str());

    // pan/tilt centre, full dimmer, shutter open, gobo 2 slot 2 indexed to +180°, prism 1 in
    let t = run(&[(1, 128), (3, 128), (38, 255), (39, 255), (37, 100), (20, 5), (21, 255), (22, 255), (23, 5)]);
    assert!(t.pan.abs() < 0.01 && t.tilt.abs() < 0.01, "{} {}", t.pan, t.tilt);
    assert_eq!((t.pan_range, t.pan_timing.fade), (540.0, 3.134));
    assert_eq!(t.dimmer, 1.0);
    assert_eq!(t.shutter, Shutter::Open);
    let gobo2 = t.gobos.iter().find(|g| g.number == 2).unwrap();
    assert_eq!((wheel_name(gobo2.wheel), gobo2.slot, gobo2.index, gobo2.rotation), (Some("RotatingGoboWheel"), 2, 180.0, None));
    let prism = &t.prisms[0];
    assert_eq!((wheel_name(prism.wheel), prism.slot), (Some("PrismWheel1"), 2));
    assert!(!ft.wheels[prism.wheel.unwrap()].slots[1].facets.is_empty());
    // Gobo2 > 0 selects the second zoom range (42°…3°), DMX 0 = widest
    assert_eq!(t.zoom, Some(42.0));

    // rotating gobo range turns the index channel into rotation speed; strobe range
    let t = run(&[(20, 35), (21, 255), (22, 255), (37, 255)]);
    let gobo2 = t.gobos.iter().find(|g| g.number == 2).unwrap();
    assert_eq!((gobo2.slot, gobo2.rotation), (3, Some(56.63)));
    assert!(matches!(t.shutter, Shutter::Strobe { random: false, .. } | Shutter::Open));
    let t = run(&[(37, 70)]);
    assert!(matches!(t.shutter, Shutter::Strobe { hz, random: false } if hz > 0.3 && hz < 20.0), "{:?}", t.shutter);
    // gobo 2 open (DMX 0) selects the narrow zoom range
    assert_eq!(run(&[]).zoom, Some(21.0));
    // full cyan
    assert_eq!(run(&[(7, 255)]).color, glam::Vec3::new(0.0, 1.0, 1.0));
}

#[test]
fn every_profile_resolves_defaults() {
    use vol_light::dmx::resolve::Profile;
    use vol_light::fixture_type::{FixtureType, GoboLibrary};
    for path in files("mvr") {
        let scene = mvr::Scene::load(&path).unwrap();
        for f in &scene.fixtures {
            let mut file = gdtf_io::load(scene.gdtf_bytes(f).unwrap().to_vec()).unwrap();
            let ft = FixtureType::from_gdtf(&mut file, &f.gdtf_mode, &mut GoboLibrary::default()).unwrap();
            let profile = Profile::from_gdtf(&file, &f.gdtf_mode, &ft).unwrap();
            let raw = profile.read(&f.addresses, |_| None);
            let t = profile.resolve(&raw, f.invert_pan, f.invert_tilt);
            assert!(t.dimmer.is_finite() && t.pan.is_finite() && t.tilt.is_finite());
        }
    }
}
