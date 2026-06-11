//! Isolated probe: does the mdns module answer A queries for <name>.local?
//! Run, then from another terminal/device: `ping probe-test.local` or a raw
//! mDNS query against 224.0.0.251:5353.

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let _guard = lclhst::mdns::announce("probe-test", 4433)?;
    eprintln!("announcing probe-test.local; Ctrl-C to stop");
    loop {
        std::thread::park();
    }
}
