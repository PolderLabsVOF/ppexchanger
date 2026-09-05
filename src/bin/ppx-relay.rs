//! Standalone bounded ppx relay. Bind to a public address only when protected
//! by your normal host firewall/rate limits; relay payloads remain opaque.
use ppexchanger::net::relay::{serve, RelayOptions};
use std::env;
use std::net::TcpListener;
use std::process;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn usage() {
    println!("ppx-relay [--bind ADDRESS] [--max-clients N] [--max-per-ip N]\n\nDefault bind: 127.0.0.1:47393\nUse --bind 0.0.0.0:47393 only for deliberate public deployment.");
}
fn main() {
    let mut bind = "127.0.0.1:47393".to_owned();
    let mut options = RelayOptions::default();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                usage();
                return;
            }
            "--version" | "-V" => {
                println!("ppx-relay {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--bind" => {
                bind = args
                    .next()
                    .unwrap_or_else(|| fail("--bind needs an address"))
            }
            "--max-clients" => options.max_clients = parse(args.next(), "--max-clients"),
            "--max-per-ip" => options.max_per_ip = parse(args.next(), "--max-per-ip"),
            _ => fail(&format!("unknown option: {arg}")),
        }
    }
    if options.max_clients == 0 || options.max_per_ip == 0 {
        fail("limits must be greater than zero");
    }
    let listener =
        TcpListener::bind(&bind).unwrap_or_else(|e| fail(&format!("cannot bind {bind}: {e}")));
    let stop = Arc::new(AtomicBool::new(false));
    let signal_stop = stop.clone();
    // No signal crate: Ctrl-C terminates normally; this guard keeps the API
    // available for embedding and avoids platform-specific dependencies.
    let _ = signal_stop;
    if let Err(e) = serve(listener, options, stop) {
        eprintln!("relay failed: {e}");
        process::exit(1);
    }
}
fn parse(value: Option<String>, option: &str) -> usize {
    value
        .unwrap_or_else(|| fail(&format!("{option} needs a number")))
        .parse()
        .unwrap_or_else(|_| fail(&format!("{option} needs a positive integer")))
}
fn fail(message: &str) -> ! {
    eprintln!("ppx-relay: {message}");
    process::exit(2)
}
