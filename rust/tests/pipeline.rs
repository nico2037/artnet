//! Art-Net → MVR patch → GDTF profile → motion model, end to end over loopback.

use std::net::{Ipv4Addr, UdpSocket};
use std::path::Path;
use std::time::{Duration, Instant};

use artnet_protocol::{ArtCommand, Output};
use vol_light::dmx::artnet::{Config, Receiver};
use vol_light::dmx::resolve::Profile;
use vol_light::dmx::UniverseMap;
use vol_light::fixture_type::{FixtureType, GoboLibrary};
use vol_light::io::{gdtf, mvr};
use vol_light::sim::motion::Motion;

#[test]
fn console_pan_reaches_fixture_with_its_real_fade() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../testdata/external/basic_gdtf.mvr");
    if !path.exists() {
        println!("basic_gdtf.mvr not fetched, skipped");
        return;
    }
    let mut scene = mvr::Scene::load(&path).unwrap();
    // the example rig is unpatched: put the first MegaPointe on universe 1, address 1
    let fixture = &mut scene.fixtures[0];
    fixture.addresses = vec![mvr::Address { dmx_break: 0, universe: 1, channel: 1 }];
    let fixture = scene.fixtures[0].clone();
    let mut file = gdtf::load(scene.gdtf_bytes(&fixture).unwrap().to_vec()).unwrap();
    let ft = FixtureType::from_gdtf(&mut file, &fixture.gdtf_mode, &mut GoboLibrary::default()).unwrap();
    let profile = Profile::from_gdtf(&file, &fixture.gdtf_mode, &ft).unwrap();

    let map = UniverseMap::default();
    let receiver = Receiver::start(Config { port: 0, universes: vec![map.to_artnet(1).unwrap()], ..Config::default() }).unwrap();
    let console = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut data = vec![0u8; 512];
    // pan full (16 bit), dimmer full, shutter open (DMX 0 = closed on the MegaPointe)
    data[0] = 255;
    data[1] = 255;
    let dimmer = profile.channels.iter().find(|c| c.name.ends_with("_Dimmer")).unwrap();
    for &o in &dimmer.offsets {
        data[o as usize - 1] = 255;
    }
    let shutter = profile.channels.iter().find(|c| c.name.ends_with("_Shutter1")).unwrap();
    data[shutter.offsets[0] as usize - 1] = 100;
    let packet = Output { port_address: 0.into(), data: data.into(), ..Output::default() };
    console
        .send_to(&ArtCommand::Output(packet).write_to_buffer().unwrap(), (Ipv4Addr::LOCALHOST, receiver.local_addr.port()))
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    let raw = loop {
        let st = receiver.state();
        if st.dmx_packets > 0 {
            break profile.read(&fixture.addresses, |u| map.to_artnet(u as i32).and_then(|p| st.dmx(p)));
        }
        drop(st);
        assert!(Instant::now() < deadline, "no ArtDmx received");
        std::thread::sleep(Duration::from_millis(5));
    };
    let target = profile.resolve(&raw, fixture.invert_pan, fixture.invert_tilt);
    assert_eq!(target.pan, 270.0);
    println!("pan timing {:?} range {}", target.pan_timing, target.pan_range);

    // MegaPointe: RealFade 3.134 s for 540°, RealAcceleration 0.549 s
    assert_eq!((target.pan_timing.fade, target.pan_timing.acceleration, target.pan_range), (3.134, 0.549, 540.0));
    // from pan 0: cruise at 540/3.134 °/s plus one acceleration time for the two ramps
    let mut motion = Motion::new(&vol_light::dmx::resolve::Target { pan: 0.0, ..target.clone() });
    let dt = 1.0 / 60.0;
    let mut t = 0.0;
    let mut actual = motion.step(&target, &ft, dt);
    while actual.pan < 270.0 && t < 5.0 {
        actual = motion.step(&target, &ft, dt);
        t += dt;
    }
    let expected = 270.0 / (540.0 / 3.134) + 0.549;
    assert!((t - expected).abs() < 0.05 * expected, "pan took {t} s, expected {expected}");
    assert!(actual.intensity > 0.99);
}
