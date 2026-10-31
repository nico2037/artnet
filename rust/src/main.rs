//! Volumetric moving head demo: automatic timeline, or --manual for direct keyboard control.

mod demo;
// patch-driven parts (universe map, set_universes, dmx lookup) get their callers with MVR and the DMX resolver
#[allow(dead_code)]
mod dmx;
mod fixtures;
mod gl_util;
mod meshes;
mod renderer;
mod resources;
mod window;

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use glam::Mat4;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::WindowId;

use dmx::artnet;
use demo::{ManualControl, EYE, FOV, HEIGHT, PAN_SPEED, TARGET, WIDTH};
use fixtures::Fixture;
use renderer::Renderer;

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
}

fn projection((w, h): (i32, i32)) -> Mat4 {
    Mat4::perspective_rh_gl(FOV.to_radians(), w as f32 / h as f32, 0.1, 200.0)
}

struct Running {
    win: window::GlWin,
    renderer: Renderer,
    fixtures: Vec<Fixture>,
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
        let (mut fixtures, rows) = demo::create_rig(self.args.fixtures);
        let quality = renderer::quality(&self.args.quality).unwrap();
        let mut renderer = Renderer::new(gl, size, quality, &self.args.gobo_dir, &demo::build_stage(&rows))?;
        let view = Mat4::look_at_rh(EYE, TARGET, glam::Vec3::Y);
        let proj = projection(size);

        if let Some(path) = &self.args.screenshot {
            demo::animate(&mut fixtures, self.args.time, renderer.gobo_count);
            renderer.render(&fixtures, EYE, view, proj, self.args.time)?;
            resources::save_png(path, (size.0 as u32, size.1 as u32), &renderer.read_screen())
                .with_context(|| format!("saving {}", path.display()))?;
            event_loop.exit();
            return Ok(());
        }
        if let Some(frames) = self.args.bench {
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
        let artnet = if self.args.artnet {
            let receiver = artnet::Receiver::start(artnet::Config {
                interface: self.args.artnet_ip,
                universes: self.args.artnet_universes.clone(),
                ..artnet::Config::default()
            })?;
            println!("Art-Net: listening on {}", receiver.local_addr);
            Some(receiver)
        } else {
            None
        };
        let now = Instant::now();
        self.running = Some(Running {
            win,
            renderer,
            fixtures,
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
        let beams = r.renderer.render(&r.fixtures, EYE, r.view, r.proj, t)?;
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
