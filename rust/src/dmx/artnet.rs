//! Art-Net receiver: ArtDmx into universe buffers, ArtPoll answered with ArtPollReply.
//!
//! One background thread owns the UDP socket. The render loop reads universes through `Receiver::state()`.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use artnet_protocol::{ArtCommand, Output, PollReply};
use socket2::{Domain, Protocol, Socket, Type};

pub const ARTNET_PORT: u16 = 6454;
pub const DMX_CHANNELS: usize = 512;
/// A universe counts as live while packets arrive at least this often.
pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(3);

const READ_TIMEOUT: Duration = Duration::from_millis(100);
const RATE_WINDOW: Duration = Duration::from_secs(1);
const PORTS_PER_REPLY: usize = 4;
const OEM_UNKNOWN: [u8; 2] = [0x00, 0xff];
/// ESTA code reserved for prototypes.
const ESTA_PROTOTYPE: u16 = 0x7ff0;
/// Indicators normal, Port-Address set by front panel / config.
const STATUS_1: u8 = 0b1101_0000;
/// Supports 15-bit Port-Address (Art-Net 3+).
const STATUS_2: u8 = 0b0000_1000;
const PORT_TYPE_DMX_OUTPUT: u8 = 0x80;
const GOOD_OUTPUT_DATA: u8 = 0x80;

#[derive(Clone, Debug)]
pub struct Config {
    /// Interface address. `0.0.0.0` listens on all interfaces. On Windows the socket binds to this address;
    /// elsewhere it binds to `0.0.0.0` (binding to a unicast address there would drop broadcasts)
    /// and the address is only reported in ArtPollReply.
    pub interface: Ipv4Addr,
    pub port: u16,
    /// Port-Addresses announced in ArtPollReply, normally the universes of the patch.
    pub universes: Vec<u16>,
    pub short_name: String,
    pub long_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interface: Ipv4Addr::UNSPECIFIED,
            port: ARTNET_PORT,
            universes: vec![0],
            short_name: "vol_light".into(),
            long_name: "vol_light Art-Net visualizer".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Universe {
    pub data: [u8; DMX_CHANNELS],
    /// Channels carried by the last packet; the rest keep their previous value.
    pub len: usize,
    pub sequence: u8,
    pub source: IpAddr,
    pub last_packet: Instant,
    pub packets: u64,
}

#[derive(Debug)]
pub struct State {
    pub universes: BTreeMap<u16, Universe>,
    /// Last packet time per sender.
    pub sources: BTreeMap<IpAddr, Instant>,
    pub dmx_packets: u64,
    pub polls: u64,
    /// ArtDmx packets dropped as out of order.
    pub dropped: u64,
    /// Datagrams on the port that are no Art-Net or could not be parsed.
    pub invalid: u64,
    /// ArtDmx packets per second over the last second.
    pub dmx_rate: f32,
    pub announced: Vec<u16>,
}

impl State {
    fn new(announced: Vec<u16>) -> Self {
        Self {
            universes: BTreeMap::new(),
            sources: BTreeMap::new(),
            dmx_packets: 0,
            polls: 0,
            dropped: 0,
            invalid: 0,
            dmx_rate: 0.0,
            announced,
        }
    }

    pub fn dmx(&self, port_address: u16) -> Option<&[u8; DMX_CHANNELS]> {
        self.universes.get(&port_address).map(|u| &u.data)
    }

    pub fn live_sources(&self, now: Instant) -> usize {
        self.sources.values().filter(|&&t| now - t < SOURCE_TIMEOUT).count()
    }

    fn apply_dmx(&mut self, output: &Output, source: IpAddr, now: Instant) {
        let port = u16::from(output.port_address);
        // the parser keeps padding bytes; the Length field is authoritative
        let data = output.data.as_ref();
        let len = (*output.length as usize).min(data.len()).min(DMX_CHANNELS);
        self.dmx_packets += 1;
        self.sources.insert(source, now);
        let universe = self.universes.entry(port).or_insert_with(|| Universe {
            data: [0; DMX_CHANNELS],
            len: 0,
            sequence: 0,
            source,
            last_packet: now,
            packets: 0,
        });
        if universe.source == source && is_stale(universe.sequence, output.sequence) {
            self.dropped += 1;
            return;
        }
        universe.data[..len].copy_from_slice(&data[..len]);
        universe.len = len;
        universe.sequence = output.sequence;
        universe.source = source;
        universe.last_packet = now;
        universe.packets += 1;
    }
}

/// Art-Net sequence numbers run 1..=255 (0 = disabled). A packet slightly behind the last one
/// arrived out of order; a large jump backwards is a restarted sender and is accepted.
fn is_stale(last: u8, sequence: u8) -> bool {
    if last == 0 || sequence == 0 {
        return false;
    }
    let behind = last.wrapping_sub(sequence);
    (1..=64).contains(&behind)
}

/// ArtPollReply packets for the announced universes. Each reply carries up to 4 ports that share
/// Net and Sub-Net (bits 14-4 of the Port-Address); several replies are told apart by BindIndex.
pub fn poll_replies(config: &Config, universes: &[u16], ip: Ipv4Addr, report_counter: u64) -> Vec<PollReply> {
    let mut sorted: Vec<u16> = universes.iter().map(|u| u & 0x7fff).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut groups: Vec<Vec<u16>> = Vec::new();
    for u in sorted {
        match groups.last_mut() {
            Some(g) if g.len() < PORTS_PER_REPLY && g[0] >> 4 == u >> 4 => g.push(u),
            _ => groups.push(vec![u]),
        }
    }
    if groups.is_empty() {
        groups.push(Vec::new());
    }
    let report = format!("#0001 [{:04}] {} universes", report_counter % 10_000, universes.len());
    groups
        .iter()
        .enumerate()
        .map(|(i, ports)| {
            let base = ports.first().copied().unwrap_or(0);
            let mut reply = PollReply {
                address: ip,
                port: ARTNET_PORT,
                version: [0, 1],
                port_address: [(base >> 8) as u8 & 0x7f, (base >> 4) as u8 & 0x0f],
                oem: OEM_UNKNOWN,
                status_1: STATUS_1,
                esta_code: ESTA_PROTOTYPE,
                short_name: fixed(&config.short_name),
                long_name: fixed(&config.long_name),
                node_report: fixed(&report),
                num_ports: [0, ports.len() as u8],
                bind_ip: ip.octets(),
                bind_index: i as u8 + 1,
                status_2: STATUS_2,
                ..PollReply::default()
            };
            for (p, &u) in ports.iter().enumerate() {
                reply.port_types[p] = PORT_TYPE_DMX_OUTPUT;
                reply.good_output[p] = GOOD_OUTPUT_DATA;
                reply.swout[p] = (u & 0x0f) as u8;
            }
            reply
        })
        .collect()
}

/// Null-terminated fixed-length string field.
fn fixed<const N: usize>(text: &str) -> [u8; N] {
    let mut out = [0; N];
    let bytes = text.as_bytes();
    let n = bytes.len().min(N - 1);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

/// Local address the OS would use to reach `peer`; no packet is sent.
fn local_ip_towards(peer: IpAddr) -> Option<Ipv4Addr> {
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    probe.connect((peer, ARTNET_PORT)).ok()?;
    match probe.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }
}

fn open_socket(config: &Config) -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // share the port with a console or another visualizer on the same machine
    socket.set_reuse_address(true)?;
    #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
    socket.set_reuse_port(true)?;
    socket.set_broadcast(true)?;
    let bind_ip = if cfg!(windows) { config.interface } else { Ipv4Addr::UNSPECIFIED };
    let addr = SocketAddrV4::new(bind_ip, config.port);
    socket.bind(&addr.into()).with_context(|| format!("binding Art-Net socket to {addr}"))?;
    let socket: UdpSocket = socket.into();
    socket.set_read_timeout(Some(READ_TIMEOUT))?;
    Ok(socket)
}

pub struct Receiver {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub local_addr: SocketAddr,
}

impl Receiver {
    pub fn start(config: Config) -> Result<Self> {
        let socket = open_socket(&config)?;
        let local_addr = socket.local_addr()?;
        let state = Arc::new(Mutex::new(State::new(config.universes.clone())));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new().name("artnet".into()).spawn({
            let (state, stop) = (state.clone(), stop.clone());
            move || receive_loop(socket, config, state, stop)
        })?;
        Ok(Self { state, stop, thread: Some(thread), local_addr })
    }

    /// Locked view of universes and statistics. Hold it only briefly: the receive thread waits on it.
    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Port-Addresses announced in ArtPollReply, e.g. after loading a new patch.
    pub fn set_universes(&self, universes: Vec<u16>) {
        self.state().announced = universes;
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn receive_loop(socket: UdpSocket, config: Config, state: Arc<Mutex<State>>, stop: Arc<AtomicBool>) {
    let mut buf = [0u8; 1024];
    let (mut window_start, mut window_packets) = (Instant::now(), 0u64);
    let mut replies_sent = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let received = socket.recv_from(&mut buf);
        let now = Instant::now();
        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
        if now - window_start >= RATE_WINDOW {
            st.dmx_rate = window_packets as f32 / (now - window_start).as_secs_f32();
            (window_start, window_packets) = (now, 0);
        }
        let (len, src) = match received {
            Ok(r) => r,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => continue,
            Err(e) => {
                // e.g. ICMP port unreachable after a reply on Windows; keep listening
                eprintln!("Art-Net receive error: {e}");
                continue;
            }
        };
        match ArtCommand::from_buffer(&buf[..len]) {
            Ok(ArtCommand::Output(output)) => {
                window_packets += 1;
                st.apply_dmx(&output, src.ip(), now);
            }
            Ok(ArtCommand::Poll(_)) => {
                st.polls += 1;
                let announced = st.announced.clone();
                drop(st);
                let ip = match config.interface {
                    Ipv4Addr::UNSPECIFIED => local_ip_towards(src.ip()).unwrap_or(Ipv4Addr::LOCALHOST),
                    ip => ip,
                };
                for reply in poll_replies(&config, &announced, ip, replies_sent) {
                    replies_sent += 1;
                    let sent = ArtCommand::PollReply(Box::new(reply))
                        .write_to_buffer()
                        .map_err(|e| anyhow::anyhow!("{e:?}"))
                        .and_then(|bytes| Ok(socket.send_to(&bytes, src)?));
                    if let Err(e) = sent {
                        eprintln!("Art-Net: ArtPollReply to {src} failed: {e}");
                    }
                }
            }
            // other nodes' replies, sync, timecode, ...
            Ok(_) => {}
            Err(_) => st.invalid += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dmx_packet(universe: u16, sequence: u8, data: Vec<u8>) -> Output {
        Output { sequence, port_address: universe.try_into().unwrap(), data: data.into(), ..Output::default() }
    }

    /// Round trip through the wire format so `length` is set like on a real packet.
    fn parsed(output: Output) -> Output {
        let bytes = ArtCommand::Output(output).write_to_buffer().unwrap();
        match ArtCommand::from_buffer(&bytes).unwrap() {
            ArtCommand::Output(o) => o,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dmx_lands_in_universe_without_padding() {
        let mut st = State::new(vec![]);
        let src: IpAddr = Ipv4Addr::new(10, 0, 0, 5).into();
        st.apply_dmx(&parsed(dmx_packet(0x123, 1, vec![10, 20, 30])), src, Instant::now());
        let u = &st.universes[&0x123];
        assert_eq!(&u.data[..4], &[10, 20, 30, 0]);
        // odd lengths are padded on the wire
        assert_eq!(u.len, 4);
        assert_eq!(st.live_sources(Instant::now()), 1);
    }

    #[test]
    fn out_of_order_packets_are_dropped_but_restarts_accepted() {
        assert!(!is_stale(0, 5));
        assert!(!is_stale(5, 0));
        assert!(!is_stale(5, 6));
        assert!(!is_stale(255, 1));
        assert!(is_stale(6, 5));
        assert!(is_stale(1, 255));
        assert!(!is_stale(200, 3));

        let mut st = State::new(vec![]);
        let src: IpAddr = Ipv4Addr::LOCALHOST.into();
        let now = Instant::now();
        st.apply_dmx(&parsed(dmx_packet(0, 10, vec![1, 1])), src, now);
        st.apply_dmx(&parsed(dmx_packet(0, 9, vec![2, 2])), src, now);
        assert_eq!(st.dmx(0).unwrap()[0], 1);
        assert_eq!(st.dropped, 1);
        // another sender is not compared against this one's sequence
        st.apply_dmx(&parsed(dmx_packet(0, 3, vec![3, 3])), Ipv4Addr::new(10, 0, 0, 9).into(), now);
        assert_eq!(st.dmx(0).unwrap()[0], 3);
    }

    #[test]
    fn poll_replies_group_four_ports_per_subnet() {
        let config = Config::default();
        let ip = Ipv4Addr::new(192, 168, 1, 20);
        let replies = poll_replies(&config, &[0, 1, 2, 3, 4, 0x15, 0x123, 1], ip, 0);
        assert_eq!(replies.len(), 4);
        assert_eq!(replies[0].num_ports, [0, 4]);
        assert_eq!(replies[0].swout, [0, 1, 2, 3]);
        assert_eq!(replies[1].num_ports, [0, 1]);
        assert_eq!(replies[1].swout[0], 4);
        // 0x15 has Sub-Net 1, 0x123 Net 1 / Sub-Net 2
        assert_eq!(replies[2].num_ports, [0, 1]);
        assert_eq!(replies[2].port_address, [0, 1]);
        assert_eq!(replies[2].swout[0], 5);
        assert_eq!(replies[3].port_address, [1, 2]);
        assert_eq!(replies.iter().map(|r| r.bind_index).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert!(replies.iter().all(|r| r.address == ip && r.bind_ip == ip.octets()));
    }

    #[test]
    fn poll_reply_for_net_and_empty_patch() {
        let config = Config::default();
        let reply = &poll_replies(&config, &[0x123], Ipv4Addr::LOCALHOST, 0)[0];
        assert_eq!(reply.port_address, [1, 2]);
        assert_eq!(reply.swout[0], 3);
        let empty = poll_replies(&config, &[], Ipv4Addr::LOCALHOST, 0);
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].num_ports, [0, 0]);
        assert_eq!(&empty[0].short_name[..10], b"vol_light\0");
    }

    #[test]
    fn receiver_takes_dmx_and_answers_poll_over_udp() {
        // port 0 is not allowed for the real node, but gives every test run its own free port
        let receiver = Receiver::start(Config { universes: vec![7], port: 0, ..Config::default() }).unwrap();
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, receiver.local_addr.port()));
        let console = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        console.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        let dmx = ArtCommand::Output(dmx_packet(7, 1, (0..=255).collect())).write_to_buffer().unwrap();
        console.send_to(&dmx, target).unwrap();
        let poll = ArtCommand::Poll(Default::default()).write_to_buffer().unwrap();
        console.send_to(&poll, target).unwrap();

        let mut buf = [0u8; 1024];
        let (len, _) = console.recv_from(&mut buf).expect("ArtPollReply");
        let ArtCommand::PollReply(reply) = ArtCommand::from_buffer(&buf[..len]).unwrap() else { panic!() };
        assert_eq!(reply.swout[0], 7);
        assert_eq!(reply.address, Ipv4Addr::LOCALHOST);

        let st = receiver.state();
        assert_eq!(st.dmx(7).unwrap()[200], 200);
        assert_eq!((st.dmx_packets, st.polls), (1, 1));
    }
}
