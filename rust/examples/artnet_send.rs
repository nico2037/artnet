//! Art-Net test sender: animated ArtDmx on a few universes, optionally an ArtPoll first.
//!
//! `cargo run --example artnet_send -- --universes 0,1 --poll`

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use artnet_protocol::{ArtCommand, Output, Poll};
use clap::Parser;

#[derive(Parser)]
struct Args {
    /// Destination; the default broadcast reaches every program on port 6454.
    #[arg(long, default_value = "255.255.255.255:6454")]
    target: SocketAddr,
    /// Art-Net Port-Addresses to send.
    #[arg(long, value_delimiter = ',', default_value = "0")]
    universes: Vec<u16>,
    /// Packets per second and universe.
    #[arg(long, default_value_t = 40.0)]
    rate: f32,
    /// Stop after this many seconds (0 = run until Ctrl+C).
    #[arg(long, default_value_t = 0.0)]
    seconds: f32,
    /// Send an ArtPoll and print the replies before sending DMX.
    #[arg(long)]
    poll: bool,
    /// Static values instead of the animation: CHANNEL=VALUE pairs (1-based), e.g. 1=128,6=255.
    /// The same values go to every universe; all other channels are 0.
    #[arg(long, value_delimiter = ',', value_parser = parse_set)]
    set: Vec<(usize, u8)>,
}

fn parse_set(text: &str) -> Result<(usize, u8), String> {
    let (ch, v) = text.split_once('=').ok_or("expected CHANNEL=VALUE")?;
    let ch: usize = ch.trim().parse().map_err(|e| format!("{e}"))?;
    if !(1..=512).contains(&ch) {
        return Err("channel must be 1..=512".into());
    }
    Ok((ch, v.trim().parse().map_err(|e| format!("{e}"))?))
}

/// Every channel follows a slow sine with its own phase, so 16-bit pairs and single channels both move visibly.
fn frame(universe: u16, t: f32) -> Vec<u8> {
    (0..512)
        .map(|ch| {
            let phase = ch as f32 * 0.37 + universe as f32 * 1.3;
            ((0.5 + 0.5 * (t * 0.8 + phase).sin()) * 255.0).round() as u8
        })
        .collect()
}

fn fixed(set: &[(usize, u8)]) -> Vec<u8> {
    let mut data = vec![0; 512];
    for &(ch, v) in set {
        data[ch - 1] = v;
    }
    data
}

fn poll(socket: &UdpSocket, target: SocketAddr) -> Result<()> {
    let bytes = ArtCommand::Poll(Poll::default()).write_to_buffer().map_err(|e| anyhow!("{e:?}"))?;
    socket.send_to(&bytes, target)?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut buf = [0u8; 1024];
    let mut found = 0;
    while let Ok((len, from)) = socket.recv_from(&mut buf) {
        if let Ok(ArtCommand::PollReply(reply)) = ArtCommand::from_buffer(&buf[..len]) {
            let name = String::from_utf8_lossy(&reply.short_name);
            let base = (reply.port_address[0] as u16) << 8 | (reply.port_address[1] as u16) << 4;
            let ports: Vec<u16> =
                reply.swout.iter().take(reply.num_ports[1] as usize).map(|&s| base | s as u16).collect();
            println!("{from}: {} bind {} universes {ports:?}", name.trim_end_matches('\0'), reply.bind_index);
            found += 1;
        }
    }
    println!("{found} ArtPollReply received");
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    if args.poll {
        poll(&socket, args.target)?;
    }
    let period = Duration::from_secs_f32(1.0 / args.rate.max(1.0));
    let start = Instant::now();
    let mut sequence = 0u8;
    println!("sending universes {:?} to {} at {} Hz", args.universes, args.target, args.rate);
    loop {
        let t = start.elapsed().as_secs_f32();
        if args.seconds > 0.0 && t >= args.seconds {
            return Ok(());
        }
        // 1..=255, 0 would switch sequencing off
        sequence = sequence % 255 + 1;
        for &u in &args.universes {
            let output = Output {
                sequence,
                port_address: u.try_into().map_err(|e| anyhow!("universe {u}: {e:?}"))?,
                data: if args.set.is_empty() { frame(u, t) } else { fixed(&args.set) }.into(),
                ..Output::default()
            };
            let bytes = ArtCommand::Output(output).write_to_buffer().map_err(|e| anyhow!("{e:?}"))?;
            socket.send_to(&bytes, args.target)?;
        }
        std::thread::sleep(period.saturating_sub(start.elapsed() - Duration::from_secs_f32(t)));
    }
}
