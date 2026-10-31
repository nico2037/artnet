//! Volumetric moving head visualizer: demo timeline, --manual keyboard control, or an MVR rig driven by Art-Net.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use glam::{vec3, Mat4, Vec2, Vec3};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::WindowId;

use vol_light::camera::{OrbitCamera, View};
use vol_light::demo::{ManualControl, EYE, FOV, HEIGHT, PAN_SPEED, TARGET, WIDTH};
use vol_light::dmx::{artnet, UniverseMap};
use vol_light::fixtures::Fixture;
use vol_light::io::mvr::Scene;
use vol_light::meshes::{interleave, quad};
use vol_light::renderer::{self, Renderer};
use vol_light::rig::{self, Rig};
use vol_light::ui::{self, Action, Panel};
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
    /// Render one frame at --time into this PNG and exit (MVR: after --time seconds of live input).
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
    /// An MVR file can also be dropped onto the window.
    #[arg(long)]
    mvr: Option<PathBuf>,
    /// MVR universe U is Art-Net Port-Address U − FIRST (default: MVR 1 = Art-Net 0).
    #[arg(long, default_value_t = 1)]
    first_mvr_universe: i32,
    /// Start with the side panel hidden (Tab toggles it).
    #[arg(long)]
    hide_panel: bool,
    /// Fixture (index in the patch list) shown in the panel's DMX monitor at start.
    #[arg(long)]
    select: Option<usize>,
}

fn projection((w, h): (i32, i32)) -> Mat4 {
    Mat4::perspective_rh_gl(FOV.to_radians(), w as f32 / h as f32, 0.1, 200.0)
}

/// A loaded MVR: the rig and its static geometry (scene objects plus a floor).
fn load_mvr(path: &Path) -> Result<(Rig, Vec<f32>)> {
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
    Ok((rig, stage))
}

/// Floor quad under the rig, padded around the fixtures.
fn floor(fixtures: &[Fixture], out: &mut Vec<f32>) {
    let (lo, hi) = rig::bounds(fixtures).unwrap_or((Vec3::ZERO, Vec3::ZERO));
    let pad = 15.0;
    let (x0, x1, z0, z1) = (lo.x - pad, hi.x + pad, lo.z - pad, hi.z + pad);
    let y = lo.y.min(0.0);
    interleave(&quad([vec3(x0, y, z0), vec3(x1, y, z0), vec3(x1, y, z1), vec3(x0, y, z1)], Vec3::Y), 0.3, out);
}

/// FOH view onto the fixtures (and the floor below them).
fn fit_camera(camera: &mut OrbitCamera, fixtures: &[Fixture]) {
    if let Some((lo, hi)) = rig::bounds(fixtures) {
        camera.set_view(View::Foh);
        camera.fit(lo, hi);
    }
}

fn patch_ports(rig: &Rig, map: UniverseMap) -> Vec<u16> {
    rig.universes().iter().filter_map(|&u| map.to_artnet(u as i32)).collect()
}

struct Egui {
    ctx: egui::Context,
    state: egui_winit::State,
    painter: egui_glow::Painter,
}

#[derive(Default)]
struct Mouse {
    pos: Option<Vec2>,
    orbit: bool,
    pan: bool,
    shift: bool,
}

struct Running {
    win: window::GlWin,
    renderer: Renderer,
    egui: Egui,
    panel: Panel,
    fixtures: Vec<Fixture>,
    rig: Option<Rig>,
    camera: OrbitCamera,
    proj: Mat4,
    mouse: Mouse,
    manual: Option<ManualControl>,
    keys: HashSet<KeyCode>,
    start: Instant,
    prev: Instant,
    last_title: Instant,
    frames: u32,
    fps: f32,
    beams: usize,
    pan_offset: f32,
    tilt_offset: f32,
    artnet: Option<artnet::Receiver>,
    artnet_hint_shown: bool,
    screenshot_done: bool,
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
        let (win, gl) = window::create(event_loop, (WIDTH, HEIGHT), "vol_light")?;
        let gl = Arc::new(gl);
        let size = win.window.inner_size();
        let size = (size.width as i32, size.height as i32);
        let quality = renderer::quality(&self.args.quality).unwrap();
        let map = UniverseMap { first_mvr_universe: self.args.first_mvr_universe };
        let mut camera = OrbitCamera::look_at(EYE, TARGET, FOV);
        let (mut fixtures, mut renderer, rig, universes) = if let Some(path) = &self.args.mvr {
            let (rig, stage) = load_mvr(path)?;
            let mut renderer = Renderer::new(gl.clone(), size, quality, &rig.gobos.layers, &stage)?;
            renderer.set_models(&rig.meshes);
            let fixtures = rig.render_fixtures();
            fit_camera(&mut camera, &fixtures);
            let ports = patch_ports(&rig, map);
            (fixtures, renderer, Some(rig), ports)
        } else {
            let (fixtures, rows) = demo::create_rig(self.args.fixtures);
            let gobos = resources::load_gobo_dir(&self.args.gobo_dir)?;
            let renderer = Renderer::new(gl.clone(), size, quality, &gobos, &demo::build_stage(&rows))?;
            (fixtures, renderer, None, self.args.artnet_universes.clone())
        };
        // demo screenshot and benchmark keep the exact matrices of earlier measurements
        let view = Mat4::look_at_rh(EYE, TARGET, Vec3::Y);
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
            Some(self.start_artnet(universes)?)
        } else {
            None
        };

        let ctx = egui::Context::default();
        let state = egui_winit::State::new(ctx.clone(), ctx.viewport_id(), &win.window, None, None, None);
        let painter = egui_glow::Painter::new(gl, "", None, false).map_err(|e| anyhow::anyhow!("egui painter: {e}"))?;
        let mvr_path = self.args.mvr.as_ref().map_or(String::new(), |p| p.display().to_string());
        let mut panel = Panel::new(map, self.args.target_fps, mvr_path);
        panel.open = !self.args.hide_panel;
        panel.selected = self.args.select;

        let now = Instant::now();
        self.running = Some(Running {
            win,
            renderer,
            egui: Egui { ctx, state, painter },
            panel,
            fixtures,
            rig,
            camera,
            proj,
            mouse: Mouse::default(),
            manual,
            keys: HashSet::new(),
            start: now,
            prev: now,
            last_title: now,
            frames: 0,
            fps: 0.0,
            beams: 0,
            pan_offset: 0.0,
            tilt_offset: 0.0,
            artnet,
            artnet_hint_shown: false,
            screenshot_done: false,
        });
        Ok(())
    }

    fn start_artnet(&self, universes: Vec<u16>) -> Result<artnet::Receiver> {
        let receiver = artnet::Receiver::start(artnet::Config {
            interface: self.args.artnet_ip,
            universes: universes.clone(),
            ..artnet::Config::default()
        })?;
        println!("Art-Net: listening on {}, announcing Port-Addresses {universes:?}", receiver.local_addr);
        Ok(receiver)
    }

    /// Replaces the current rig (demo or MVR) with the MVR at `path`; on failure the current rig stays.
    fn load(&mut self, path: &Path) {
        let result = load_mvr(path).and_then(|(rig, stage)| {
            let artnet_missing = self.running.as_ref().is_some_and(|r| r.artnet.is_none());
            let map = self.running.as_ref().map_or(UniverseMap::default(), |r| r.panel.universe_map);
            let receiver = if artnet_missing { Some(self.start_artnet(patch_ports(&rig, map))?) } else { None };
            let Some(r) = self.running.as_mut() else { return Ok(()) };
            r.renderer.set_scene(&rig.gobos.layers, &stage)?;
            r.renderer.set_models(&rig.meshes);
            if let Some(receiver) = receiver {
                r.artnet = Some(receiver);
            }
            if let Some(a) = &r.artnet {
                a.set_universes(patch_ports(&rig, map));
            }
            r.fixtures = rig.render_fixtures();
            fit_camera(&mut r.camera, &r.fixtures);
            r.panel.message = Some((true, format!("{} fixtures loaded", rig.fixtures.len())));
            r.panel.selected = None;
            r.panel.mvr_path = path.display().to_string();
            r.manual = None;
            r.rig = Some(rig);
            Ok(())
        });
        if let Err(e) = result {
            eprintln!("{e:#}");
            if let Some(r) = self.running.as_mut() {
                r.panel.message = Some((false, format!("{e:#}")));
            }
        }
    }

    fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Load(path) => self.load(&path),
                Action::View(view) => {
                    if let Some(r) = self.running.as_mut() {
                        r.camera.set_view(view);
                    }
                }
                Action::Fit => {
                    if let Some(r) = self.running.as_mut() {
                        fit_camera(&mut r.camera, &r.fixtures);
                    }
                }
                Action::UniverseMapChanged => {
                    if let Some(r) = self.running.as_mut() {
                        if let (Some(rig), Some(a)) = (&r.rig, &r.artnet) {
                            a.set_universes(patch_ports(rig, r.panel.universe_map));
                        }
                    }
                }
            }
        }
    }

    fn frame(&mut self) -> Result<Vec<Action>> {
        let args = &self.args;
        let Some(r) = self.running.as_mut() else { return Ok(Vec::new()) };
        let now = Instant::now();
        let dt = (now - r.prev).as_secs_f32();
        r.prev = now;
        let t = (now - r.start).as_secs_f32();

        if let Some(rig) = r.rig.as_mut() {
            let map = r.panel.universe_map;
            match r.artnet.as_ref() {
                Some(a) => {
                    let st = a.state();
                    rig.update(|u| map.to_artnet(u as i32).and_then(|p| st.dmx(p)), dt);
                }
                None => rig.update(|_| None, dt),
            }
            r.fixtures = rig.render_fixtures();
            r.renderer.set_model_instances(&rig.model_instances());
        } else {
            let held = |k| r.keys.contains(&k) as i32 as f32;
            let (pan_axis, tilt_axis) = (
                held(KeyCode::ArrowRight) - held(KeyCode::ArrowLeft),
                // hanging heads: negative tilt lifts the beam toward the audience
                held(KeyCode::ArrowDown) - held(KeyCode::ArrowUp),
            );
            r.pan_offset += PAN_SPEED * dt * pan_axis;
            r.tilt_offset += PAN_SPEED * dt * tilt_axis;
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
        r.beams = r.renderer.render(&r.fixtures, r.camera.eye(), r.camera.view(), r.proj, t)?;

        // panel on top of the 3D view
        let raw_input = r.egui.state.take_egui_input(&r.win.window);
        let stats = ui::Stats {
            fps: r.fps,
            quality: &args.quality,
            volume_scale: r.renderer.volume_scale,
            beams: r.beams,
            fixtures: r.fixtures.len(),
        };
        let mut actions = Vec::new();
        let output = r.egui.ctx.run(raw_input, |ctx| {
            actions = r.panel.show(ctx, r.artnet.as_ref(), r.rig.as_ref(), &stats);
        });
        r.egui.state.handle_platform_output(&r.win.window, output.platform_output);
        let primitives = r.egui.ctx.tessellate(output.shapes, output.pixels_per_point);
        let size = r.win.window.inner_size();
        r.egui.painter.paint_and_update_textures([size.width, size.height], output.pixels_per_point, &primitives, &output.textures_delta);

        if let (Some(path), true) = (&args.screenshot, r.rig.is_some() && t >= args.time) {
            // MVR mode: DMX arrives over the network, so the shot is taken live after --time seconds
            resources::save_png(path, (size.width, size.height), &r.renderer.read_screen())
                .with_context(|| format!("saving {}", path.display()))?;
            r.screenshot_done = true;
        }
        r.win.swap()?;

        r.frames += 1;
        let elapsed = (now - r.last_title).as_secs_f32();
        if elapsed >= 0.5 {
            r.fps = r.frames as f32 / elapsed;
            if r.panel.target_fps > 0.0 {
                r.renderer.adapt_volume_scale(r.fps, r.panel.target_fps);
            }
            let mode = match (&r.rig, &r.manual) {
                (Some(rig), _) => format!("{} fixtures from MVR", rig.fixtures.len()),
                (None, Some(m)) => format!("MANUAL | {}", m.status()),
                (None, None) => "demo".to_string(),
            };
            let artnet = r.artnet.as_ref().map_or(String::new(), |a| {
                let st = a.state();
                if st.dmx_packets == 0 && !r.artnet_hint_shown && now - r.start > ARTNET_HINT_AFTER {
                    // unicast to 127.0.0.1 reaches only one of several programs sharing the port
                    println!("Art-Net: no packets yet. Send as broadcast from the console, or unicast to this machine's network address.");
                    r.artnet_hint_shown = true;
                }
                format!(" | Art-Net {:.0} pkt/s, {} sources", st.dmx_rate, st.live_sources(now))
            });
            r.win.window.set_title(&format!("vol_light | {:.0} FPS | {mode}{artnet}", r.fps));
            r.frames = 0;
            r.last_title = now;
        }
        Ok(actions)
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.error = Some(error);
        event_loop.exit();
    }

    /// Camera and shortcut input that egui did not take.
    fn scene_input(&mut self, event_loop: &ActiveEventLoop, event: &WindowEvent) {
        let Some(r) = self.running.as_mut() else { return };
        match event {
            WindowEvent::ModifiersChanged(m) => r.mouse.shift = m.state().shift_key(),
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = *state == ElementState::Pressed;
                match button {
                    MouseButton::Left if r.mouse.shift => r.mouse.pan = pressed,
                    MouseButton::Left => r.mouse.orbit = pressed,
                    MouseButton::Middle | MouseButton::Right => r.mouse.pan = pressed,
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 50.0,
                };
                r.camera.zoom(notches);
            }
            WindowEvent::KeyboardInput { event: KeyEvent { physical_key: PhysicalKey::Code(code), state, repeat, .. }, .. } => {
                match state {
                    ElementState::Pressed if *code == KeyCode::Escape => event_loop.exit(),
                    ElementState::Pressed => {
                        r.keys.insert(*code);
                        if *repeat {
                            return;
                        }
                        match code {
                            KeyCode::Tab => r.panel.open = !r.panel.open,
                            KeyCode::Digit1 => r.camera.set_view(View::Foh),
                            KeyCode::Digit2 => r.camera.set_view(View::Top),
                            KeyCode::Digit3 => r.camera.set_view(View::Left),
                            KeyCode::Digit4 => r.camera.set_view(View::Right),
                            KeyCode::Digit0 => fit_camera(&mut r.camera, &r.fixtures),
                            _ => {
                                if let Some(manual) = r.manual.as_mut() {
                                    manual.key_down(*code);
                                }
                            }
                        }
                    }
                    ElementState::Released => {
                        r.keys.remove(code);
                    }
                }
            }
            _ => {}
        }
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
        let Some(r) = self.running.as_mut() else {
            if matches!(event, WindowEvent::CloseRequested) {
                event_loop.exit();
            }
            return;
        };
        let consumed = r.egui.state.on_window_event(&r.win.window, &event).consumed;
        match &event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                // minimized windows report 0x0, which is no valid render target size
                let (Some(width), Some(height)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
                    return;
                };
                let size = (size.width as i32, size.height as i32);
                r.win.resize(width, height);
                r.renderer.resize(size);
                r.proj = projection(size);
            }
            WindowEvent::DroppedFile(path) => {
                let path = path.clone();
                self.load(&path);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let pos = Vec2::new(position.x as f32, position.y as f32);
                if let Some(prev) = r.mouse.pos {
                    let d = pos - prev;
                    if r.mouse.orbit {
                        r.camera.orbit(d.x, d.y);
                    } else if r.mouse.pan {
                        r.camera.pan(d.x, d.y, r.renderer.size.1 as f32);
                    }
                }
                r.mouse.pos = Some(pos);
            }
            WindowEvent::MouseInput { state: ElementState::Released, .. } => {
                // releases always end a drag, also over the panel
                r.mouse.orbit = false;
                r.mouse.pan = false;
            }
            WindowEvent::RedrawRequested => match self.frame() {
                Err(e) => self.fail(event_loop, e),
                Ok(actions) => {
                    self.apply(actions);
                    if self.running.as_ref().is_some_and(|r| r.screenshot_done) {
                        event_loop.exit();
                    }
                }
            },
            WindowEvent::KeyboardInput { event: KeyEvent { state: ElementState::Released, physical_key: PhysicalKey::Code(code), .. }, .. } => {
                r.keys.remove(code);
            }
            WindowEvent::ModifiersChanged(m) => r.mouse.shift = m.state().shift_key(),
            _ if !consumed && !r.egui.ctx.wants_keyboard_input() => self.scene_input(event_loop, &event),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(r) = &self.running {
            r.win.window.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(r) = self.running.as_mut() {
            r.egui.painter.destroy();
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
