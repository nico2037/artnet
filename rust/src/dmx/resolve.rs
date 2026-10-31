//! DMX → fixture attributes for one GDTF mode.
//!
//! `Profile::from_gdtf` flattens the mode into channels and functions once; `Profile::resolve`
//! reads a fixture's DMX footprint every frame and produces a `Target` (what the console asks for;
//! the motion model then decides what the fixture actually shows).

use std::num::NonZeroU8;

use anyhow::{anyhow, Result};
use gdtf::dmx_mode::{DmxBreak, DmxMode, RelationType};
use gdtf::values::{DmxValue, Name};
use gdtf::GdtfFile;
use glam::Vec3;

use crate::fixture_type::FixtureType;
use crate::io::mvr::{Address, UNIVERSE_SIZE};

/// What a channel function drives, decoded from its GDTF attribute name once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    Pan,
    Tilt,
    Dimmer,
    /// `Shutter1` open/closed by physical value (0 = closed).
    Shutter,
    Strobe,
    StrobeRandom,
    /// Strobe pulses; the physical value is a time, not a rate.
    StrobePulse,
    /// R, G, B, W, A (amber), UV is ignored.
    ColorAdd(u8),
    /// C, M, Y
    ColorSub(u8),
    Cto,
    /// `Color{n}`: slot selection on colour wheel n.
    ColorWheel(u8),
    ColorWheelSpin(u8),
    /// `Gobo{n}` (also SelectShake/SelectSpin): slot selection.
    Gobo(u8),
    GoboPos(u8),
    GoboPosRotate(u8),
    GoboWheelSpin(u8),
    Prism(u8),
    PrismPos(u8),
    PrismPosRotate(u8),
    Zoom,
    Focus,
    Iris,
    Frost,
    Other,
}

impl Feature {
    pub fn from_attribute(name: &str) -> Self {
        // `Gobo2PosRotate` → ("Gobo", 2, "PosRotate")
        let split = |prefix: &str| -> Option<(u8, &str)> {
            let rest = name.strip_prefix(prefix)?;
            let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            Some((rest[..digits].parse().ok()?, &rest[digits..]))
        };
        match name {
            "Pan" => return Self::Pan,
            "Tilt" => return Self::Tilt,
            "Dimmer" => return Self::Dimmer,
            "Zoom" => return Self::Zoom,
            "Iris" => return Self::Iris,
            "CTO" | "CTC" => return Self::Cto,
            "ColorAdd_R" | "ColorRGB_Red" => return Self::ColorAdd(0),
            "ColorAdd_G" | "ColorRGB_Green" => return Self::ColorAdd(1),
            "ColorAdd_B" | "ColorRGB_Blue" => return Self::ColorAdd(2),
            "ColorAdd_W" | "ColorRGB_White" => return Self::ColorAdd(3),
            "ColorAdd_A" | "ColorRGB_Amber" => return Self::ColorAdd(4),
            "ColorSub_C" => return Self::ColorSub(0),
            "ColorSub_M" => return Self::ColorSub(1),
            "ColorSub_Y" => return Self::ColorSub(2),
            _ => {}
        }
        if let Some((_, rest)) = split("Shutter") {
            return match rest {
                "" => Self::Shutter,
                "Strobe" => Self::Strobe,
                r if r.starts_with("StrobeRandom") => Self::StrobeRandom,
                r if r.starts_with("StrobePulse") => Self::StrobePulse,
                _ => Self::Other,
            };
        }
        if let Some((n, rest)) = split("Gobo") {
            return match rest {
                "" | "SelectShake" | "SelectSpin" => Self::Gobo(n),
                "Pos" => Self::GoboPos(n),
                "PosRotate" => Self::GoboPosRotate(n),
                "WheelSpin" => Self::GoboWheelSpin(n),
                _ => Self::Other,
            };
        }
        if let Some((n, rest)) = split("Color") {
            return match rest {
                "" => Self::ColorWheel(n),
                "WheelSpin" => Self::ColorWheelSpin(n),
                _ => Self::Other,
            };
        }
        if let Some((n, rest)) = split("Prism") {
            return match rest {
                "" => Self::Prism(n),
                "Pos" => Self::PrismPos(n),
                "PosRotate" => Self::PrismPosRotate(n),
                _ => Self::Other,
            };
        }
        match split("Focus") {
            Some((1, "")) => return Self::Focus,
            Some(_) => return Self::Other,
            None => {}
        }
        match split("Frost") {
            Some((_, "")) => Self::Frost,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChannelSet {
    pub dmx_from: u32,
    /// 1-based slot of the function's wheel.
    pub wheel_slot: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Function {
    /// GDTF name, or the attribute when the function has none.
    pub name: String,
    pub attribute: String,
    pub feature: Feature,
    /// In the channel's resolution.
    pub dmx_from: u32,
    pub default: u32,
    pub physical_from: f32,
    pub physical_to: f32,
    /// Seconds for a full-range move / to reach full speed; 0 = not given.
    pub real_fade: f32,
    pub real_acceleration: f32,
    /// Index into `FixtureType::wheels`.
    pub wheel: Option<usize>,
    /// Active only while channel `.0` (index into `Profile::channels`) is within `.1..=.2` (its resolution).
    pub mode_master: Option<(usize, u32, u32)>,
    pub sets: Vec<ChannelSet>,
    /// Relation Multiply: scaled by the normalised value of this channel.
    pub multiplied_by: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub name: String,
    /// MVR break (GDTF DMXBreak − 1).
    pub dmx_break: u32,
    /// 1-based offsets within the break, most significant byte first; empty for virtual channels.
    pub offsets: Vec<u32>,
    pub default: u32,
    pub functions: Vec<Function>,
}

impl Channel {
    pub fn bytes(&self) -> u32 {
        self.offsets.len().max(1) as u32
    }

    pub fn max(&self) -> u32 {
        (1u64 << (8 * self.bytes())) as u32 - 1
    }
}

/// GDTF `DMXValue` (`n/b`, `n/bs`) in a channel of `bytes` bytes. Without `s`, missing low bytes
/// repeat the value's bytes (byte mirroring, 255/1 = 65535/2); with `s` they are zero.
pub fn dmx_value(value: u64, value_bytes: u32, shifting: bool, bytes: u32) -> u32 {
    let value_bytes = value_bytes.clamp(1, 4);
    if value_bytes >= bytes {
        return (value >> (8 * (value_bytes - bytes))) as u32;
    }
    let mut out = value;
    for i in 0..bytes - value_bytes {
        let byte = if shifting { 0 } else { (value >> (8 * (value_bytes - 1 - i % value_bytes))) & 0xff };
        out = out << 8 | byte;
    }
    out as u32
}

fn convert(v: DmxValue, bytes: u32) -> u32 {
    dmx_value(v.value(), NonZeroU8::get(v.bytes()) as u32, v.shifting(), bytes)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Timing {
    /// Seconds for the full range (GDTF RealFade); 0 = unknown, the motion model uses defaults.
    pub fade: f32,
    pub acceleration: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shutter {
    Open,
    Closed,
    Strobe { hz: f32, random: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WheelTarget {
    /// Index into `FixtureType::wheels`; `None` if no function names the wheel.
    pub wheel: Option<usize>,
    /// Number in the attribute name (`Gobo2` → 2).
    pub number: u8,
    /// 1-based slot; 0 = not set.
    pub slot: u32,
    /// Continuous wheel rotation, degrees per second (WheelSpin).
    pub spin: f32,
    /// Indexed orientation of the slot, degrees.
    pub index: f32,
    /// Continuous slot rotation, degrees per second (PosRotate); wins over `index` when set.
    pub rotation: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub pan: f32,
    pub tilt: f32,
    /// Degrees covered by the full DMX range, for speed limits.
    pub pan_range: f32,
    pub tilt_range: f32,
    pub pan_timing: Timing,
    pub tilt_timing: Timing,
    pub dimmer: f32,
    pub shutter: Shutter,
    /// Additive/subtractive mix and CTO, linear RGB; wheels come on top.
    pub color: Vec3,
    pub color_wheels: Vec<WheelTarget>,
    pub gobos: Vec<WheelTarget>,
    pub prisms: Vec<WheelTarget>,
    /// Full beam angle in degrees; `None` = fixed beam from the GDTF.
    pub zoom: Option<f32>,
    pub focus: f32,
    /// 1 = open.
    pub iris: f32,
    pub frost: f32,
}

impl Default for Target {
    fn default() -> Self {
        Self {
            pan: 0.0,
            tilt: 0.0,
            pan_range: 0.0,
            tilt_range: 0.0,
            pan_timing: Timing::default(),
            tilt_timing: Timing::default(),
            dimmer: 0.0,
            shutter: Shutter::Open,
            color: Vec3::ONE,
            color_wheels: Vec::new(),
            gobos: Vec::new(),
            prisms: Vec::new(),
            zoom: None,
            focus: 0.0,
            iris: 1.0,
            frost: 0.0,
        }
    }
}

/// Warm tint of a full CTO (about 3200 K from 5600 K), linear RGB.
const CTO_FULL: Vec3 = Vec3::new(1.0, 0.62, 0.32);
/// Amber emitter colour, linear RGB.
const AMBER: Vec3 = Vec3::new(1.0, 0.45, 0.0);

#[derive(Clone, Debug)]
pub struct Profile {
    pub channels: Vec<Channel>,
}

impl Profile {
    pub fn from_gdtf(file: &GdtfFile, mode: &str, ft: &FixtureType) -> Result<Self> {
        let gdtf_ft = file.description.fixture_types.first().ok_or_else(|| anyhow!("GDTF without fixture type"))?;
        let dmx_mode: &DmxMode =
            gdtf_ft.dmx_mode(&Name::new_lossy(mode)).ok_or_else(|| anyhow!("no mode \"{mode}\""))?;
        let names: Vec<String> = dmx_mode.dmx_channels.iter().map(|c| c.name().to_string()).collect();
        let mut channels = Vec::new();
        for c in &dmx_mode.dmx_channels {
            let offsets: Vec<u32> = c.offset.clone().unwrap_or_default().into_iter().map(|o| o.max(1) as u32).collect();
            let bytes = offsets.len().max(1) as u32;
            let dmx_break = match c.dmx_break {
                DmxBreak::Value(b) => (b.max(1) - 1) as u32,
                DmxBreak::Overwrite => 0,
            };
            let default = c.initial_function().map(|(_, f)| convert(f.default, bytes)).unwrap_or(0);
            let mut functions = Vec::new();
            for logical in &c.logical_channels {
                for f in &logical.channel_functions {
                    let attribute = f.attribute.first().map(|a| a.to_string()).unwrap_or_default();
                    let wheel_name = f.wheel.as_ref().and_then(|w| w.first()).map(|w| w.to_string());
                    let wheel = wheel_name.and_then(|w| ft.wheels.iter().position(|x| x.name == w));
                    let mode_master = f.mode_master.as_ref().and_then(|m| {
                        let master = names.iter().position(|n| m.node.first().is_some_and(|x| x.as_ref() == n))?;
                        let master_bytes = dmx_mode.dmx_channels[master].offset.as_ref().map_or(1, |o| o.len().max(1)) as u32;
                        Some((master, convert(m.from, master_bytes), convert(m.to, master_bytes)))
                    });
                    let mut sets: Vec<ChannelSet> = f
                        .channel_sets
                        .iter()
                        .map(|s| ChannelSet {
                            dmx_from: convert(s.dmx_from, bytes),
                            // the crate stores WheelSlotIndex 0-based (GDTF 1 = first slot, 0 = none)
                            wheel_slot: s.wheel_slot_index.filter(|&i| i >= 0).map(|i| i as u32 + 1),
                        })
                        .collect();
                    sets.sort_by_key(|s| s.dmx_from);
                    functions.push(Function {
                        name: f.name.as_ref().map(|n| n.to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| attribute.clone()),
                        feature: Feature::from_attribute(&attribute),
                        attribute,
                        dmx_from: convert(f.dmx_from, bytes),
                        default: convert(f.default, bytes),
                        physical_from: f.physical_from as f32,
                        physical_to: f.physical_to as f32,
                        real_fade: f.real_fade as f32,
                        real_acceleration: f.real_acceleration as f32,
                        wheel,
                        mode_master,
                        sets,
                        multiplied_by: None,
                    });
                }
            }
            channels.push(Channel { name: c.name().to_string(), dmx_break, offsets, default, functions });
        }
        for relation in &dmx_mode.relations {
            if relation.type_ != RelationType::Multiply {
                continue;
            }
            let master = names.iter().position(|n| relation.master.first().is_some_and(|x| x.as_ref() == n));
            // follower: Channel.LogicalChannel.ChannelFunction
            let follower = relation.follower.first().and_then(|c| names.iter().position(|n| n == c.as_ref()));
            let function = relation.follower.last().map(|f| f.to_string());
            if let (Some(master), Some(follower), Some(function)) = (master, follower, function) {
                for f in &mut channels[follower].functions {
                    if f.name == function {
                        f.multiplied_by = Some(master);
                    }
                }
            }
        }
        Ok(Self { channels })
    }

    /// Raw channel values; channels whose universe has no data yet keep their default.
    pub fn read<'a>(&self, addresses: &[Address], dmx: impl Fn(u32) -> Option<&'a [u8; 512]>) -> Vec<u32> {
        self.channels
            .iter()
            .map(|c| {
                let Some(base) = addresses.iter().find(|a| a.dmx_break == c.dmx_break) else { return c.default };
                if c.offsets.is_empty() {
                    return c.default;
                }
                let mut value = 0u32;
                for &offset in &c.offsets {
                    // absolute 0-based address; a footprint may run into the next universe
                    let abs = (base.universe - 1) * UNIVERSE_SIZE + base.channel - 1 + offset - 1;
                    let Some(data) = dmx(abs / UNIVERSE_SIZE + 1) else { return c.default };
                    value = value << 8 | data[(abs % UNIVERSE_SIZE) as usize] as u32;
                }
                value
            })
            .collect()
    }

    /// Index of the active function of `channel` for raw values `raw`.
    pub fn active_function(&self, channel: usize, raw: &[u32]) -> Option<usize> {
        let value = raw[channel];
        self.channels[channel]
            .functions
            .iter()
            .enumerate()
            .filter(|(_, f)| f.mode_master.is_none_or(|(m, from, to)| (from..=to).contains(&raw[m])))
            .filter(|(_, f)| f.dmx_from <= value)
            .max_by_key(|(i, f)| (f.dmx_from, *i))
            .map(|(i, _)| i)
    }

    /// Physical value of the active function, interpolated within its DMX range.
    pub fn physical(&self, channel: usize, function: usize, raw: &[u32]) -> f32 {
        let c = &self.channels[channel];
        let f = &c.functions[function];
        let value = raw[channel];
        // the range ends where the next applicable function starts
        let end = c
            .functions
            .iter()
            .filter(|g| g.mode_master == f.mode_master && g.dmx_from > f.dmx_from)
            .map(|g| g.dmx_from - 1)
            .min()
            .unwrap_or(c.max());
        let t = if end > f.dmx_from { (value - f.dmx_from) as f32 / (end - f.dmx_from) as f32 } else { 0.0 };
        let mut physical = f.physical_from + (f.physical_to - f.physical_from) * t.clamp(0.0, 1.0);
        if let Some(master) = f.multiplied_by {
            physical *= raw[master] as f32 / self.channels[master].max() as f32;
        }
        physical
    }

    pub fn wheel_slot(&self, channel: usize, function: usize, raw: &[u32]) -> Option<u32> {
        let value = raw[channel];
        self.channels[channel].functions[function].sets.iter().rev().find(|s| s.dmx_from <= value).and_then(|s| s.wheel_slot)
    }

    /// Target state from raw channel values (see `read`).
    pub fn resolve(&self, raw: &[u32], invert_pan: bool, invert_tilt: bool) -> Target {
        let mut t = Target::default();
        let (mut add, mut add_used, mut sub, mut cto) = ([0.0f32; 5], false, [0.0f32; 3], 0.0f32);
        let mut raw = raw.to_vec();
        for (i, c) in self.channels.iter().enumerate() {
            let inverted = c.functions.iter().any(|f| {
                (f.feature == Feature::Pan && invert_pan) || (f.feature == Feature::Tilt && invert_tilt)
            });
            if inverted {
                raw[i] = c.max() - raw[i];
            }
        }
        for i in 0..self.channels.len() {
            let Some(fi) = self.active_function(i, &raw) else { continue };
            let f = &self.channels[i].functions[fi];
            let p = self.physical(i, fi, &raw);
            let span = (f.physical_to - f.physical_from).abs();
            // position within the physical range, for attributes whose units vary between GDTFs
            let fraction = if span > 0.0 { (p - f.physical_from.min(f.physical_to)) / span } else { p.clamp(0.0, 1.0) };
            let timing = Timing { fade: f.real_fade, acceleration: f.real_acceleration };
            match f.feature {
                Feature::Pan => (t.pan, t.pan_range, t.pan_timing) = (p, span, timing),
                Feature::Tilt => (t.tilt, t.tilt_range, t.tilt_timing) = (p, span, timing),
                Feature::Dimmer => t.dimmer = p.clamp(0.0, 1.0),
                Feature::Shutter => t.shutter = if p > 0.0 { Shutter::Open } else { Shutter::Closed },
                Feature::Strobe => t.shutter = Shutter::Strobe { hz: p, random: false },
                Feature::StrobeRandom => t.shutter = Shutter::Strobe { hz: p, random: true },
                // pulse time in seconds → about one flash per pulse length
                Feature::StrobePulse => t.shutter = Shutter::Strobe { hz: 1.0 / p.max(0.05), random: false },
                Feature::ColorAdd(k) => {
                    add[k as usize] = fraction;
                    add_used = true;
                }
                Feature::ColorSub(k) => sub[k as usize] = fraction,
                Feature::Cto => cto = fraction,
                Feature::ColorWheel(n) => wheel(&mut t.color_wheels, f.wheel, n).slot = self.wheel_slot(i, fi, &raw).unwrap_or(0),
                Feature::ColorWheelSpin(n) => wheel(&mut t.color_wheels, f.wheel, n).spin = p,
                Feature::Gobo(n) => wheel(&mut t.gobos, f.wheel, n).slot = self.wheel_slot(i, fi, &raw).unwrap_or(0),
                Feature::GoboPos(n) => wheel(&mut t.gobos, f.wheel, n).index = p,
                Feature::GoboPosRotate(n) => wheel(&mut t.gobos, f.wheel, n).rotation = Some(p),
                Feature::GoboWheelSpin(n) => wheel(&mut t.gobos, f.wheel, n).spin = p,
                Feature::Prism(n) => wheel(&mut t.prisms, f.wheel, n).slot = self.wheel_slot(i, fi, &raw).unwrap_or(0),
                Feature::PrismPos(n) => wheel(&mut t.prisms, f.wheel, n).index = p,
                Feature::PrismPosRotate(n) => wheel(&mut t.prisms, f.wheel, n).rotation = Some(p),
                Feature::Zoom => t.zoom = Some(p),
                Feature::Focus => t.focus = fraction,
                Feature::Iris => t.iris = 1.0 - fraction,
                Feature::Frost => t.frost = t.frost.max(fraction),
                Feature::Other => {}
            }
        }
        if add_used {
            let [r, g, b, w, a] = add;
            t.color = (Vec3::new(r, g, b) + Vec3::splat(w) + AMBER * a).min(Vec3::ONE);
        }
        t.color *= Vec3::ONE - Vec3::from(sub);
        t.color *= Vec3::ONE.lerp(CTO_FULL, cto);
        for list in [&mut t.color_wheels, &mut t.gobos, &mut t.prisms] {
            list.sort_by_key(|w| w.number);
        }
        t
    }
}

fn wheel(list: &mut Vec<WheelTarget>, wheel: Option<usize>, number: u8) -> &mut WheelTarget {
    let index = match list.iter().position(|w| w.number == number) {
        Some(i) => i,
        None => {
            list.push(WheelTarget { wheel, number, slot: 0, spin: 0.0, index: 0.0, rotation: None });
            list.len() - 1
        }
    };
    let w = &mut list[index];
    w.wheel = w.wheel.or(wheel);
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dmx_values_mirror_shift_and_reduce() {
        assert_eq!(dmx_value(255, 1, false, 2), 65535);
        assert_eq!(dmx_value(255, 1, true, 2), 65280);
        assert_eq!(dmx_value(0x12, 1, false, 3), 0x121212);
        assert_eq!(dmx_value(0x1234, 2, false, 3), 0x123412);
        assert_eq!(dmx_value(0x1234, 2, false, 1), 0x12);
        assert_eq!(dmx_value(128, 1, false, 1), 128);
        assert_eq!(dmx_value(32768, 2, false, 2), 32768);
    }

    #[test]
    fn attribute_names_decode() {
        assert_eq!(Feature::from_attribute("Gobo2PosRotate"), Feature::GoboPosRotate(2));
        assert_eq!(Feature::from_attribute("Gobo1SelectShake"), Feature::Gobo(1));
        assert_eq!(Feature::from_attribute("Gobo1WheelSpin"), Feature::GoboWheelSpin(1));
        assert_eq!(Feature::from_attribute("Color1"), Feature::ColorWheel(1));
        assert_eq!(Feature::from_attribute("ColorAdd_W"), Feature::ColorAdd(3));
        assert_eq!(Feature::from_attribute("Shutter1Strobe"), Feature::Strobe);
        assert_eq!(Feature::from_attribute("Shutter1StrobePulseOpen"), Feature::StrobePulse);
        assert_eq!(Feature::from_attribute("Prism1Pos"), Feature::PrismPos(1));
        assert_eq!(Feature::from_attribute("Frost2"), Feature::Frost);
        assert_eq!(Feature::from_attribute("Focus1"), Feature::Focus);
        assert_eq!(Feature::from_attribute("Focus1Distance"), Feature::Other);
        assert_eq!(Feature::from_attribute("ColorMixReset"), Feature::Other);
        assert_eq!(Feature::from_attribute("Goboz"), Feature::Other);
    }

    fn function(attribute: &str, dmx_from: u32, physical: (f32, f32)) -> Function {
        Function {
            name: attribute.into(),
            attribute: attribute.into(),
            feature: Feature::from_attribute(attribute),
            dmx_from,
            default: 0,
            physical_from: physical.0,
            physical_to: physical.1,
            real_fade: 0.0,
            real_acceleration: 0.0,
            wheel: None,
            mode_master: None,
            sets: Vec::new(),
            multiplied_by: None,
        }
    }

    fn channel(name: &str, offsets: &[u32], functions: Vec<Function>) -> Channel {
        Channel { name: name.into(), dmx_break: 0, offsets: offsets.to_vec(), default: 0, functions }
    }

    /// Pan 16 bit, gobo with slots, gobo index/rotation under a mode master, dimmer, CMY.
    fn profile() -> Profile {
        let mut gobo_select = function("Gobo1", 0, (0.0, 1.0));
        gobo_select.wheel = Some(0);
        gobo_select.sets = vec![
            ChannelSet { dmx_from: 0, wheel_slot: Some(1) },
            ChannelSet { dmx_from: 10, wheel_slot: Some(2) },
            ChannelSet { dmx_from: 20, wheel_slot: Some(3) },
        ];
        let mut gobo_rotating = function("Gobo1", 128, (0.0, 1.0));
        gobo_rotating.wheel = Some(0);
        gobo_rotating.sets = vec![ChannelSet { dmx_from: 128, wheel_slot: Some(2) }];
        let mut index = function("Gobo1Pos", 0, (-180.0, 180.0));
        index.mode_master = Some((1, 0, 127));
        let mut rotate = function("Gobo1PosRotate", 0, (-100.0, 100.0));
        rotate.mode_master = Some((1, 128, 255));
        Profile {
            channels: vec![
                channel("Yoke_Pan", &[1, 2], vec![function("Pan", 0, (-270.0, 270.0))]),
                channel("Head_Gobo1", &[3], vec![gobo_select, gobo_rotating]),
                channel("Head_Gobo1Pos", &[4], vec![index, rotate]),
                channel("Head_Dimmer", &[5, 6], vec![function("Dimmer", 0, (0.0, 1.0))]),
                channel("Head_C", &[7], vec![function("ColorSub_C", 0, (0.0, 1.0))]),
                channel("Head_Shutter", &[8], vec![function("Shutter1", 0, (0.0, 0.0)), function("Shutter1", 32, (1.0, 1.0)), function("Shutter1Strobe", 64, (1.0, 20.0))]),
            ],
        }
    }

    fn dmx(values: &[(usize, u8)]) -> [u8; 512] {
        let mut d = [0u8; 512];
        for &(ch, v) in values {
            d[ch - 1] = v;
        }
        d
    }

    fn resolve(p: &Profile, data: [u8; 512]) -> Target {
        let at = [Address { dmx_break: 0, universe: 1, channel: 1 }];
        p.resolve(&p.read(&at, |u| (u == 1).then_some(&data)), false, false)
    }

    #[test]
    fn pan_16_bit_and_dimmer() {
        let p = profile();
        let t = resolve(&p, dmx(&[(1, 128), (2, 0), (5, 255), (6, 255), (8, 40)]));
        assert!((t.pan - 270.0 * (32768.0 / 65535.0 * 2.0 - 1.0)).abs() < 1e-3, "{}", t.pan);
        assert_eq!(t.pan_range, 540.0);
        assert_eq!(t.dimmer, 1.0);
        assert_eq!(t.shutter, Shutter::Open);
        let t = resolve(&p, dmx(&[(1, 255), (2, 255)]));
        assert_eq!(t.pan, 270.0);
        assert_eq!(t.shutter, Shutter::Closed);
    }

    #[test]
    fn wheel_sets_and_mode_master() {
        let p = profile();
        let t = resolve(&p, dmx(&[(3, 15), (4, 255)]));
        assert_eq!(t.gobos.len(), 1);
        assert_eq!((t.gobos[0].slot, t.gobos[0].index, t.gobos[0].rotation), (2, 180.0, None));
        // rotating range of the gobo channel switches the index channel to rotation
        let t = resolve(&p, dmx(&[(3, 130), (4, 255)]));
        assert_eq!(t.gobos[0].slot, 2);
        assert_eq!(t.gobos[0].rotation, Some(100.0));
        assert_eq!(t.gobos[0].wheel, Some(0));
    }

    #[test]
    fn strobe_range_interpolates_to_its_end() {
        let p = profile();
        let t = resolve(&p, dmx(&[(8, 255)]));
        assert_eq!(t.shutter, Shutter::Strobe { hz: 20.0, random: false });
        let t = resolve(&p, dmx(&[(8, 64)]));
        assert_eq!(t.shutter, Shutter::Strobe { hz: 1.0, random: false });
    }

    #[test]
    fn cmy_subtracts_and_missing_universe_keeps_defaults() {
        let p = profile();
        let t = resolve(&p, dmx(&[(7, 255)]));
        assert_eq!(t.color, Vec3::new(0.0, 1.0, 1.0));
        let at = [Address { dmx_break: 0, universe: 1, channel: 1 }];
        let raw = p.read(&at, |_| None);
        assert!(raw.iter().zip(&p.channels).all(|(r, c)| *r == c.default));
    }

    #[test]
    fn invert_and_footprint_across_universes() {
        let p = profile();
        let mut first = [0u8; 512];
        first[511] = 255; // pan MSB on 1.512
        let second = dmx(&[(1, 255)]); // pan LSB on 2.1
        let at = [Address { dmx_break: 0, universe: 1, channel: 512 }];
        let raw = p.read(&at, |u| match u {
            1 => Some(&first),
            2 => Some(&second),
            _ => None,
        });
        assert_eq!(raw[0], 65535);
        assert_eq!(p.resolve(&raw, true, false).pan, -270.0);
    }

    #[test]
    fn relation_multiply_scales_follower() {
        let mut p = profile();
        p.channels[3].functions[0].multiplied_by = Some(4);
        let t = resolve(&p, dmx(&[(5, 255), (6, 255), (7, 51)]));
        assert!((t.dimmer - 0.2).abs() < 1e-6);
    }
}
