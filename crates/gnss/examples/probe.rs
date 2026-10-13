//! Run the receiver loop against a real port and print what it sees, once a
//! second — the check for a newly plugged-in receiver, without the client.
//!
//! ```bash
//! cargo run -p cartopolis_gnss --features serial --example probe -- [PORT] [BAUD] [SECONDS] [RATE_HZ]
//! ```
//!
//! PORT empty (`""`) or absent picks the likeliest USB serial device. Sends the
//! same output commands the client does; nothing is saved in the receiver.
//!
//! Corrections come from a caster when `NTRIP_HOST` is set, with `NTRIP_PORT`
//! (2101; 443 means TLS, as does `NTRIP_TLS=1`), `NTRIP_MOUNT` (empty: the
//! nearest station), `NTRIP_USER` and `NTRIP_PASS`; the rover's position is
//! sent to it. `NTRIP_NEAR=lat,lng` stands in for a position while the
//! receiver has no fix, to choose the station from (the client uses the map).
//! With `NTRIP_LIST=1` the probe prints the caster's sourcetable and stops.

use std::time::{Duration, Instant};

use cartopolis_gnss::{ntrip, serial, Caster, Config, Receiver};

fn caster() -> Option<Caster> {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let host = env("NTRIP_HOST");
    (!host.is_empty()).then(|| Caster {
        host,
        port: env("NTRIP_PORT").parse().unwrap_or(2101),
        mountpoint: env("NTRIP_MOUNT"),
        username: env("NTRIP_USER"),
        password: env("NTRIP_PASS"),
        send_position: true,
        tls: env("NTRIP_TLS") == "1",
    })
}

fn main() {
    let mut args = std::env::args().skip(1);
    let port = args.next().unwrap_or_default();
    let baud: u32 = args.next().and_then(|b| b.parse().ok()).unwrap_or(115_200);
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(15);
    let rate_hz: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);

    let caster = caster();
    if std::env::var("NTRIP_LIST").is_ok_and(|v| v == "1") {
        let Some(c) = &caster else {
            eprintln!("NTRIP_LIST needs NTRIP_HOST");
            std::process::exit(2);
        };
        match ntrip::sourcetable(c) {
            Ok(mounts) => {
                for m in mounts {
                    println!(
                        "{:<20} {:<12} {:>9.4} {:>9.4}  position {}  {}",
                        m.name,
                        m.format,
                        m.lat,
                        m.lng,
                        if m.wants_position { "wanted" } else { "-" },
                        m.identifier
                    );
                }
            }
            Err(e) => {
                eprintln!("sourcetable: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    for p in serial::ports() {
        println!(
            "port: {}{}",
            p.label,
            if p.likely { "  (likely GNSS)" } else { "" }
        );
    }
    let receiver = Receiver::start(
        Box::new(move || serial::open(&port, baud)),
        Config {
            configure: true,
            rate_hz: cartopolis_gnss::receiver::rate_for_baud(rate_hz, baud),
            caster,
        },
    );
    let near = std::env::var("NTRIP_NEAR").ok().and_then(|v| {
        let (lat, lng) = v.split_once(',')?;
        Some((lat.trim().parse().ok()?, lng.trim().parse().ok()?))
    });
    receiver.set_position_hint(near);
    let start = Instant::now();
    let mut last_sentences = 0;
    while start.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_secs(1));
        let s = receiver.status();
        let rate = s.sentences - last_sentences;
        last_sentences = s.sentences;
        let fix = s.fresh_solution(Duration::from_secs(3)).map_or_else(
            || format!("no fix ({} satellites)", s.tracking.satellites.unwrap_or(0)),
            |f| {
                format!(
                    "{} ±{:.2} m  {:.8},{:.8}  {} sats  HDOP {}  speed {:.2} m/s",
                    f.quality.label(),
                    f.accuracy_m(),
                    f.lat,
                    f.lng,
                    f.satellites.unwrap_or(0),
                    f.hdop.map_or("-".into(), |h| format!("{h:.1}")),
                    f.speed_ms.unwrap_or(0.0),
                )
            },
        );
        println!(
            "{:>3}s  {:?}  {rate} sentences/s  {}  bad {}  reply {:?}\n      {fix}",
            start.elapsed().as_secs(),
            s.link,
            s.epoch_s
                .map_or("-".into(), |e| format!("{:.1} Hz", 1.0 / e)),
            s.bad_checksums,
            s.last_reply.as_deref().unwrap_or(""),
        );
        let c = &s.corrections;
        if c.state != cartopolis_gnss::CasterState::Off {
            let age = s
                .fresh_solution(Duration::from_secs(3))
                .and_then(|f| f.correction_age_s)
                .map_or("-".into(), |a| format!("{a:.1} s"));
            println!(
                "      corrections {:?} {}{}  {} B  {} frames  messages {:?}  age {age}",
                c.state,
                c.mountpoint.as_deref().unwrap_or(""),
                if c.chosen_from_hint {
                    " (from hint)"
                } else {
                    ""
                },
                c.bytes,
                c.frames,
                c.messages.iter().map(|m| m.0).collect::<Vec<_>>(),
            );
        }
    }
}
