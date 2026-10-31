//! Prints an MVR rig's patch: fixtures, addresses and the DMX channels of each fixture type.
//!
//! `cargo run --example mvr_info -- rig.mvr [--functions]`

use std::path::PathBuf;

use anyhow::Result;
use vol_light::io::mvr::Scene;
use vol_light::rig::Rig;

fn main() -> Result<()> {
    let path: PathBuf = std::env::args_os().nth(1).ok_or_else(|| anyhow::anyhow!("usage: mvr_info FILE.mvr"))?.into();
    let functions = std::env::args().any(|a| a == "--functions");
    let scene = Scene::load(&path)?;
    let rig = Rig::from_scene(&scene);
    for (i, k) in rig.kinds.iter().enumerate() {
        println!("type {i}: {} {} / {}", k.ty.manufacturer, k.ty.name, k.ty.mode);
        for c in &k.profile.channels {
            let offsets: Vec<String> = c.offsets.iter().map(|o| o.to_string()).collect();
            println!("  break {} ch {:>7}  {}", c.dmx_break + 1, offsets.join("/"), c.name);
            if functions {
                let shift = 8 * (c.bytes() - 1);
                for f in &c.functions {
                    println!("      from {:>3}  {} ({})", f.dmx_from >> shift, f.name, f.attribute);
                    for set in &f.sets {
                        if let Some(slot) = set.wheel_slot {
                            println!("        from {:>3}  slot {slot}", set.dmx_from >> shift);
                        }
                    }
                }
            }
        }
    }
    for f in &rig.fixtures {
        let addr: Vec<String> = f.mvr.addresses.iter().map(|a| format!("break {} = {}.{}", a.dmx_break + 1, a.universe, a.channel)).collect();
        let p = f.mvr.transform.w_axis;
        println!("{} (id {}): type {}, {}, at ({:.2}, {:.2}, {:.2}) m", f.mvr.name, f.mvr.fixture_id, f.kind, addr.join(", "), p.x, p.y, p.z);
    }
    for s in &rig.skipped {
        println!("skipped: {s}");
    }
    Ok(())
}
