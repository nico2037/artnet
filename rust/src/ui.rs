//! Side panel: view and render settings, Art-Net status, patch list, DMX monitor of the selected fixture.

use std::path::PathBuf;
use std::time::Instant;

use egui::{Color32, RichText};

use crate::camera::View;
use crate::dmx::artnet;
use crate::dmx::resolve::Shutter;
use crate::dmx::UniverseMap;
use crate::rig::Rig;

pub const PANEL_WIDTH: f32 = 330.0;

pub struct Panel {
    pub open: bool,
    pub selected: Option<usize>,
    pub universe_map: UniverseMap,
    pub target_fps: f32,
    /// MVR path as edited in the panel.
    pub mvr_path: String,
    /// Result of the last load, shown under the path.
    pub message: Option<(bool, String)>,
}

/// What the user asked for this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    View(View),
    Fit,
    Load(PathBuf),
    /// MVR universe 1 now maps to another Port-Address.
    UniverseMapChanged,
}

pub struct Stats<'a> {
    pub fps: f32,
    pub quality: &'a str,
    pub volume_scale: f32,
    pub beams: usize,
    pub fixtures: usize,
}

impl Panel {
    pub fn new(universe_map: UniverseMap, target_fps: f32, mvr_path: String) -> Self {
        Self { open: true, selected: None, universe_map, target_fps, mvr_path, message: None }
    }

    pub fn show(&mut self, ctx: &egui::Context, artnet: Option<&artnet::Receiver>, rig: Option<&Rig>, stats: &Stats) -> Vec<Action> {
        let mut actions = Vec::new();
        if !self.open {
            return actions;
        }
        egui::SidePanel::right("panel").default_width(PANEL_WIDTH).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.view_section(ui, stats, &mut actions);
                if let Some(receiver) = artnet {
                    self.artnet_section(ui, receiver);
                }
                self.patch_section(ui, rig, &mut actions);
                if let (Some(rig), Some(i)) = (rig, self.selected) {
                    if let Some(f) = rig.fixtures.get(i) {
                        self.fixture_section(ui, rig, f, artnet);
                    }
                }
                ui.separator();
                ui.label(RichText::new("Tab: panel · drag: orbit · Shift/middle drag: pan · wheel: zoom · 1–4: views · 0: fit").small().weak());
            });
        });
        actions
    }

    fn view_section(&mut self, ui: &mut egui::Ui, stats: &Stats, actions: &mut Vec<Action>) {
        egui::CollapsingHeader::new("View").default_open(true).show(ui, |ui| {
            ui.label(format!(
                "{:.0} FPS · {} · haze {:.0}% · {} fixtures · {} beams",
                stats.fps,
                stats.quality,
                stats.volume_scale * 100.0,
                stats.fixtures,
                stats.beams
            ));
            ui.horizontal(|ui| {
                ui.label("Target FPS");
                ui.add(egui::Slider::new(&mut self.target_fps, 0.0..=144.0).step_by(1.0))
                    .on_hover_text("Haze resolution adapts until this frame rate is reached; 0 = fixed");
            });
            ui.horizontal(|ui| {
                for (label, view) in [("FOH", View::Foh), ("Top", View::Top), ("Left", View::Left), ("Right", View::Right)] {
                    if ui.button(label).clicked() {
                        actions.push(Action::View(view));
                    }
                }
                if ui.button("Fit").clicked() {
                    actions.push(Action::Fit);
                }
            });
        });
    }

    fn artnet_section(&mut self, ui: &mut egui::Ui, receiver: &artnet::Receiver) {
        let now = Instant::now();
        let st = receiver.state();
        egui::CollapsingHeader::new("Art-Net").default_open(true).show(ui, |ui| {
            ui.label(format!("Listening on {}", receiver.local_addr));
            let live = st.live_sources(now);
            let color = if live > 0 { Color32::LIGHT_GREEN } else { Color32::GRAY };
            ui.label(RichText::new(format!("{:.0} pkt/s · {} sources · {} universes", st.dmx_rate, live, st.universes.len())).color(color));
            ui.label(
                RichText::new(format!(
                    "{} ArtDmx, {} ArtPoll, {} out of order, {} invalid",
                    st.dmx_packets, st.polls, st.dropped, st.invalid
                ))
                .small(),
            );
            let announced: Vec<String> = st.announced.iter().map(|p| p.to_string()).collect();
            ui.label(RichText::new(format!("Announced Port-Addresses: {}", announced.join(", "))).small());
            if !st.universes.is_empty() {
                egui::Grid::new("universes").striped(true).show(ui, |ui| {
                    for h in ["Port", "MVR U", "Packets", "Source", "Age"] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    for (&port, u) in &st.universes {
                        ui.label(port.to_string());
                        ui.label(self.universe_map.to_mvr(port).to_string());
                        ui.label(u.packets.to_string());
                        ui.label(u.source.to_string());
                        ui.label(format!("{:.1} s", (now - u.last_packet).as_secs_f32()));
                        ui.end_row();
                    }
                });
            }
        });
    }

    fn patch_section(&mut self, ui: &mut egui::Ui, rig: Option<&Rig>, actions: &mut Vec<Action>) {
        egui::CollapsingHeader::new("Patch").default_open(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("MVR");
                let edit = ui.add(egui::TextEdit::singleline(&mut self.mvr_path).desired_width(170.0).hint_text("path or drop a file"));
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let label = if rig.is_some() { "Reload" } else { "Load" };
                if (ui.button(label).clicked() || enter) && !self.mvr_path.trim().is_empty() {
                    actions.push(Action::Load(PathBuf::from(self.mvr_path.trim())));
                }
            });
            if let Some((ok, text)) = &self.message {
                ui.label(RichText::new(text).small().color(if *ok { Color32::LIGHT_GREEN } else { Color32::LIGHT_RED }));
            }
            ui.horizontal(|ui| {
                ui.label("MVR universe 1 = Art-Net");
                let mut port = 1 - self.universe_map.first_mvr_universe;
                if ui.add(egui::DragValue::new(&mut port).range(0..=32767)).changed() {
                    self.universe_map.first_mvr_universe = 1 - port;
                    actions.push(Action::UniverseMapChanged);
                }
            });
            let Some(rig) = rig else {
                ui.label(RichText::new("Demo rig. Load an MVR to drive fixtures from Art-Net.").weak());
                return;
            };
            for s in &rig.skipped {
                ui.label(RichText::new(format!("skipped: {s}")).small().color(Color32::LIGHT_RED));
            }
            for n in &rig.notes {
                ui.label(RichText::new(n).small().color(Color32::YELLOW));
            }
            egui::ScrollArea::vertical().id_salt("patch").max_height(220.0).show(ui, |ui| {
                egui::Grid::new("patch_grid").striped(true).show(ui, |ui| {
                    for h in ["FID", "Name", "Type", "Address"] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    for (i, f) in rig.fixtures.iter().enumerate() {
                        let selected = self.selected == Some(i);
                        let fid = if f.mvr.fixture_id.is_empty() { "–".to_string() } else { f.mvr.fixture_id.clone() };
                        if ui.selectable_label(selected, fid).clicked() {
                            self.selected = if selected { None } else { Some(i) };
                        }
                        ui.label(&f.mvr.name);
                        let ty = &rig.kinds[f.kind].ty;
                        ui.label(&ty.name).on_hover_text(format!("{} {}\n{}", ty.manufacturer, ty.name, ty.mode));
                        ui.label(address(f));
                        ui.end_row();
                    }
                });
            });
        });
    }

    fn fixture_section(&self, ui: &mut egui::Ui, rig: &Rig, f: &crate::rig::RigFixture, artnet: Option<&artnet::Receiver>) {
        let kind = &rig.kinds[f.kind];
        egui::CollapsingHeader::new(format!("Fixture {}", f.mvr.name)).default_open(true).show(ui, |ui| {
            ui.label(RichText::new(format!("{} {} · {}", kind.ty.manufacturer, kind.ty.name, kind.ty.mode)).small());
            // DMX monitor: the fixture's footprint per break
            let st = artnet.map(|a| a.state());
            for a in &f.mvr.addresses {
                let width = kind
                    .profile
                    .channels
                    .iter()
                    .filter(|c| c.dmx_break == a.dmx_break)
                    .flat_map(|c| c.offsets.iter().copied())
                    .max()
                    .unwrap_or(0);
                let data = st.as_ref().and_then(|st| self.universe_map.to_artnet(a.universe as i32).and_then(|p| st.dmx(p)));
                ui.label(RichText::new(format!("Break {} · {}.{} · {} channels", a.dmx_break + 1, a.universe, a.channel, width)).strong());
                let values: Vec<String> = (0..width)
                    .map(|k| {
                        let ch = (a.channel + k) as usize;
                        data.and_then(|d| d.get(ch - 1)).map_or("–".into(), |v| v.to_string())
                    })
                    .collect();
                for (row, chunk) in values.chunks(8).enumerate() {
                    ui.monospace(format!("{:>3}: {}", row * 8 + 1, chunk.iter().map(|v| format!("{v:>3}")).collect::<Vec<_>>().join(" ")));
                }
            }
            drop(st);
            ui.separator();
            let t = &f.target;
            let shutter = match t.shutter {
                Shutter::Open => "open".to_string(),
                Shutter::Closed => "closed".to_string(),
                Shutter::Strobe { hz, random } => format!("strobe {hz:.1} Hz{}", if random { " random" } else { "" }),
            };
            let actual = f.actual.as_ref();
            egui::Grid::new("decoded").show(ui, |ui| {
                let mut row = |name: &str, value: String| {
                    ui.label(name);
                    ui.label(value);
                    ui.end_row();
                };
                row("Pan", format!("{:+.1}° (target {:+.1}°)", actual.map_or(t.pan, |a| a.pan), t.pan));
                row("Tilt", format!("{:+.1}° (target {:+.1}°)", actual.map_or(t.tilt, |a| a.tilt), t.tilt));
                row("Dimmer", format!("{:.0}% · shutter {shutter}", t.dimmer * 100.0));
                row("Color", format!("{:.2} {:.2} {:.2}", t.color.x, t.color.y, t.color.z));
                for (name, wheels) in [("Color wheel", &t.color_wheels), ("Gobo", &t.gobos), ("Prism", &t.prisms)] {
                    for w in wheels {
                        let motion = match w.rotation {
                            Some(speed) => format!(" · spin {speed:+.0}°/s"),
                            None if w.index != 0.0 => format!(" · index {:.0}°", w.index),
                            None => String::new(),
                        };
                        row(&format!("{name} {}", w.number), format!("slot {}{motion}", w.slot));
                    }
                }
                if let Some(zoom) = t.zoom {
                    row("Zoom", format!("{zoom:.1}°"));
                }
                row("Focus / Iris / Frost", format!("{:.2} / {:.2} / {:.2}", t.focus, t.iris, t.frost));
            });
        });
    }
}

fn address(f: &crate::rig::RigFixture) -> String {
    if f.mvr.addresses.is_empty() {
        return "unpatched".into();
    }
    f.mvr.addresses.iter().map(|a| format!("{}.{}", a.universe, a.channel)).collect::<Vec<_>>().join(", ")
}
