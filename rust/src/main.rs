//! Volumetric moving head demo: automatic timeline, or --manual for direct keyboard control.


use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use glam::{vec3, Mat4, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::WindowId;

use vol_light::dmx::{artnet, UniverseMap};
use vol_light::io::mvr::Scene;
use vol_light::meshes::{interleave, quad};
use vol_light::rig::{self, Rig};
use vol_light::demo::{ManualControl, EYE, FOV, HEIGHT, PAN_SPEED, TARGET, WIDTH};
use vol_light::fixtures::Fixture;
use vol_light::renderer::{self, Renderer};
use vol_light::{demo, resources, window};

/// Volumetric moving head visualizer: demo timeline, manual control, Art-Net input.
#[derive(Parser)]
#[command(name = "vol_light")]
struct Args {
    #[arg(long, default_value = "medium", value_parser = ["low", "medium", "high"])]
    quality: String,
    /// Number of fixtures in the demo rig.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(usize))]
    fixtures: usize,
    /// Render one frame at --time into this PNG and exit.
    #[arg(long)]
    screenshot: Option<PathBuf>,
    #[arg(long, default_value_t = 18.0)]
    time: f32,
    /// Keyboard control instead of the demo timeline.
    #[arg(long)]
    manual: bool,
    /// Lower the haze resolution until this frame rate is reached (0 = fixed).
    #[arg(long, default_value_t = 60.0)]
    target_fps: f32,
    /// Measure FRAMES frames from --time and print ms/frame.
    #[arg(long, value_name = "FRAMES")]
    bench: Option<usize>,
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/../gobos"))]
    gobo_dir: PathBuf,
    /// Receive Art-Net (UDP 6454) and answer ArtPoll.
    #[arg(long)]
    artnet: bool,
    /// Interface address for Art-Net; 0.0.0.0 = all interfaces.
    #[arg(long, default_value_t = Ipv4Addr::UNSPECIFIED)]
    artnet_ip: Ipv4Addr,
    /// Art-Net Port-Addresses announced in ArtPollReply until a patch is loaded.
    #[arg(long, value_delimiter = ',', default_value = "0")]
    artnet_universes: Vec<u16>,
    /// Load this MVR rig instead of the demo; DMX comes from Art-Net (implies --artnet).
    #[arg(long)]
    mvr: Option<PathBuf>,
    /// Art-Net Port-Address of MVR universe 1 is 0 by default; MVR universe U maps to U - FIRST.
    #[arg(long, default_value_t = 1)]
    first_mvr_universe: i32,
}

fn projection((w, h): (i32, i32)) -> Mat4 {
    Mat4::perspective_rh_gl(FOV.to_radians(), w as f32 / h as f32, 0.1, 200.0)
}

struct Running {
    win: window::GlWin,
    renderer: Renderer,
    fixtures: Vec<Fixture>,
    rig: Option<Rig>,
    eye: Vec3,
    view: Mat4,
    proj: Mat4,
    manual: Option<ManualControl>,
    keys: HashSet<KeyCode>,
    start: Instant,
    prev: Instant,
    last_title: Instant,
    frames: u32,
    pan_offset: f32,
    tilt_offset: f32,
    artnet: Option<artnet::Receiver>,
    artnet_hint_shown: bool,
    screenshot_done: bool,
}

/// Camera framing a rig: in front of (+Z) and above the fixtures' bounding box.
fn frame_rig(fixtures: &[Fixture]) -> (Vec3, Vec3) {
    let Some((lo, hi)) = rig::bounds(fixtures) else { return (EYE, TARGET) };
    let center = (lo + hi) * 0.5;
    let extent = (hi - lo).max_element().max(4.0);
    let target = vec3(center.x, hi.y * 0.4, center.z);
    let eye = vec3(center.x, hi.y * 0.6 + 1.5, hi.z + extent * 0.9 + 4.0);
    (eye, target)
}

/// Floor quad under the rig, padded around the fixtures.
fn floor(fixtures: &[Fixture], out: &mut Vec<f32>) {
    let (lo, hi) = rig::bounds(fixtures).unwrap_or((Vec3::ZERO, Vec3::ZERO));
    let pad = 15.0;
    let (x0, x1, z0, z1) = (lo.x - pad, hi.x + pad, lo.z - pad, hi.z + pad);
    let y = lo.y.min(0.0);
    interleave(&quad([vec3(x0, y, z0), vec3(x1, y, z0), vec3(x1, y, z1), vec3(x0, y, z1)], Vec3::Y), 0.3, out);
}

/// Art-Net is idle this long before the console gets a hint about unicast to 127.0.0.1.
const ARTNET_HINT_AFTER: Duration = Duration::from_secs(5);

struct App {
    args: Args,
    running: Option<Running>,
    error: Option<anyhow::Error>,
}

impl App {
    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let (win, gl) = window::create(event_loop, (WIDTH, HEIGHT), "Movinghead Demo")?;
        let size = win.window.inner_size();
        let size = (size.width as i32, size.height as i32);
        let quality = renderer::quality(&self.args.quality).unwrap();
        let map = UniverseMap { first_mvr_universe: self.args.first_mvr_universe };
        let (mut fixtures, mut renderer, rig, eye, target, universes) = if let Some(path) = &self.args.mvr {
            let scene = Scene::load(path).with_context(|| format!("loading {}", path.display()))?;
            let rig = Rig::from_scene(&scene);
            for s in &rig.skipped {
                eprintln!("MVR: skipped {s}");
            }
            for n in &rig.notes {
                eprintln!("MVR: {n}");
            }
            let fixtures = rig.render_fixtures();
            let (mut stage, failed) = rig::scene_vertices(&scene);
            for f in failed {
                eprintln!("MVR: scene mesh {f}");
            }
            floor(&fixtures, &mut stage);
            println!(
                "MVR: {} fixtures ({} types), {} scene meshes, {} gobo layers",
                rig.fixtures.len(),
                rig.kinds.len(),
                scene.meshes.len(),
                rig.gobos.layers.len()
            );
            let renderer = Renderer::new(gl, size, quality, &rig.gobos.layers, &stage)?;
            let (eye, target) = frame_rig(&fixtures);
            let universes: Vec<u16> = rig.universes().iter().filter_map(|&u| map.to_artnet(u as i32)).collect();
            (fixtures, renderer, Some(rig), eye, target, universes)
        } else {
            let (fixtures, rows) = demo::create_rig(self.args.fixtures);
            let gobos = resources::load_gobo_dir(&self.args.gobo_dir)?;
            let renderer = Renderer::new(gl, size, quality, &gobos, &demo::build_stage(&rows))?;
            (fixtures, renderer, None, EYE, TARGET, self.args.artnet_universes.clone())
        };
        let view = Mat4::look_at_rh(eye, target, Vec3::Y);
        let proj = projection(size);

        if let (Some(path), None) = (&self.args.screenshot, &rig) {
            demo::animate(&mut fixtures, self.args.time, renderer.gobo_count);
            renderer.render(&fixtures, EYE, view, proj, self.args.time)?;
            resources::save_png(path, (size.0 as u32, size.1 as u32), &renderer.read_screen())
                .with_context(|| format!("saving {}", path.display()))?;
            event_loop.exit();
            return Ok(());
        }
        if let (Some(frames), None) = (self.args.bench, &rig) {
            // same protocol as the Python measurements: 30 warm-up frames, then t advances 1/60 s per frame
            let mut t0 = Instant::now();
            for i in 0..30 + frames {
                if i == 30 {
                    renderer.finish();
                    t0 = Instant::now();
                }
                let t = self.args.time + i as f32 / 60.0;
                demo::animate(&mut fixtures, t, renderer.gobo_count);
                renderer.render(&fixtures, EYE, view, proj, t)?;
                win.swap()?;
            }
            renderer.finish();
            let ms = t0.elapsed().as_secs_f64() * 1000.0 / frames as f64;
            println!("{} fixtures {} {:.2} ms/frame ({:.0} FPS)", fixtures.len(), self.args.quality, ms, 1000.0 / ms);
            event_loop.exit();
            return Ok(());
        }

        let manual = self.args.manual.then(|| ManualControl::new(renderer.gobo_count));
        if manual.is_some() {
            println!("{}", demo::MANUAL_HELP);
        }
        let artnet = if self.args.artnet || rig.is_some() {
            let receiver = artnet::Receiver::start(artnet::Config {
                interface: self.args.artnet_ip,
                universes,
                ..artnet::Config::default()
            })?;
            println!("Art-Net: listening on {}", receiver.local_addr);
            if rig.is_some() {
                let st = receiver.state();
                let ports: Vec<String> = st.announced.iter().map(|p| p.to_string()).collect();
                println!("Art-Net: patch uses Port-Addresses {}", ports.join(", "));
            }
            Some(receiver)
        } else {
            None
        };
        let now = Instant::now();
        self.running = Some(Running {
            win,
            renderer,
            fixtures,
            rig,
            eye,
            view,
            proj,
            manual,
            keys: HashSet::new(),
            start: now,
            prev: now,
            last_title: now,
            frames: 0,
            pan_offset: 0.0,
            tilt_offset: 0.0,
            artnet,
            artnet_hint_shown: false,
            screenshot_done: false,
        });
        Ok(())
    }

    fn frame(&mut self) -> Result<()> {
        let args = &self.args;
        let Some(r) = self.running.as_mut() else { return Ok(()) };
        let now = Instant::now();
        let dt = (now - r.prev).as_secs_f32();
        r.prev = now;
        let held = |k| r.keys.contains(&k) as i32 as f32;
        let (pan_axis, tilt_axis) = (
            held(KeyCode::ArrowRight) - held(KeyCode::ArrowLeft),
            // hanging heads: negative tilt lifts the beam toward the audience
            held(KeyCode::ArrowDown) - held(KeyCode::ArrowUp),
        );
        r.pan_offset += PAN_SPEED * dt * pan_axis;
        r.tilt_offset += PAN_SPEED * dt * tilt_axis;

        let t = (now - r.start).as_secs_f32();
        if let Some(rig) = r.rig.as_mut() {
            let map = UniverseMap { first_mvr_universe: args.first_mvr_universe };
            match r.artnet.as_ref() {
                Some(a) => {
                    let st = a.state();
                    rig.update(|u| map.to_artnet(u as i32).and_then(|p| st.dmx(p)), dt);
                }
                None => rig.update(|_| None, dt),
            }
            r.fixtures = rig.render_fixtures();
        } else {
            if let Some(manual) = r.manual.as_mut() {
                manual.update(&r.keys, dt);
                manual.apply(&mut r.fixtures);
            } else {
                demo::animate(&mut r.fixtures, t, r.renderer.gobo_count);
            }
            for f in &mut r.fixtures {
                f.pan += r.pan_offset;
                f.tilt += r.tilt_offset;
            }
        }
        let beams = r.renderer.render(&r.fixtures, r.eye, r.view, r.proj, t)?;
        if let (Some(path), true) = (&args.screenshot, r.rig.is_some() && t >= args.time) {
            // MVR mode: DMX arrives over the network, so the shot is taken live after --time seconds
            let size = r.win.window.inner_size();
            resources::save_png(path, (size.width, size.height), &r.renderer.read_screen())
                .with_context(|| format!("saving {}", path.display()))?;
            r.screenshot_done = true;
        }
        r.win.swap()?;

        r.frames += 1;
        let elapsed = (now - r.last_title).as_secs_f32();
        if elapsed >= 0.5 {
            let fps = r.frames as f32 / elapsed;
            if args.target_fps > 0.0 {
                r.renderer.adapt_volume_scale(fps, args.target_fps);
            }
            let mode = r.manual.as_ref().map_or("demo".to_string(), |m| format!("MANUAL | {}", m.status()));
            let artnet = r.artnet.as_ref().map_or(String::new(), |a| {
                let st = a.state();
                if st.dmx_packets == 0 && !r.artnet_hint_shown && now - r.start > ARTNET_HINT_AFTER {
                    // unicast to 127.0.0.1 reaches only one of several programs sharing the port
                    println!("Art-Net: no packets yet. Send as broadcast from the console, or unicast to this machine's network address.");
                    r.artnet_hint_shown = true;
                }
                format!(
                    " | Art-Net {:.0} pkt/s, {} universes, {} sources",
                    st.dmx_rate,
                    st.universes.len(),
                    st.live_sources(now)
                )
            });
            r.win.window.set_title(&format!(
                "Movinghead Demo | {fps:.0} FPS | {} haze {:.0}% | {} fixtures, {beams} beams | pan {:+.0}° tilt {:+.0}° | {mode}{artnet}",
                args.quality,
                r.renderer.volume_scale * 100.0,
                r.fixtures.len(),
                r.pan_offset,
                r.tilt_offset
            ));
            r.frames = 0;
            r.last_title = now;
        }
        Ok(())
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.error = Some(error);
        event_loop.exit();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.running.is_none() {
            if let Err(e) = self.init(event_loop) {
                self.fail(event_loop, e);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                let Some(r) = self.running.as_mut() else { return };
                // minimized windows report 0x0, which is no valid render target size
                let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
                    return;
                };
                let size = (size.width as i32, size.height as i32);
                r.win.resize(width, height);
                r.renderer.resize(size);
                r.proj = projection(size);
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { physical_key: PhysicalKey::Code(code), state, repeat, .. }, ..
            } => {
                let Some(r) = self.running.as_mut() else { return };
                match state {
                    ElementState::Pressed if code == KeyCode::Escape => event_loop.exit(),
                    ElementState::Pressed => {
                        r.keys.insert(code);
                        if let (false, Some(manual)) = (repeat, r.manual.as_mut()) {
                            manual.key_down(code);
                        }
                    }
                    ElementState::Released => {
                        r.keys.remove(&code);
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.frame() {
                    self.fail(event_loop, e);
                } else if self.running.as_ref().is_some_and(|r| r.screenshot_done) {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(r) = &self.running {
            r.win.window.request_redraw();
        }
    }
}

fn main() -> Result<()> {
    let mut args = Args::parse();
    args.fixtures = args.fixtures.max(1);
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App { args, running: None, error: None };
    event_loop.run_app(&mut app)?;
    app.error.map_or(Ok(()), Err)
}
