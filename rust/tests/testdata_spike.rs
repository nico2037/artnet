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
