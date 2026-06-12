use std::io::IsTerminal;

use clap::{Parser, Subcommand};
use tokio::sync::oneshot;

#[derive(Parser)]
#[command(
    name = "lclhst",
    version,
    about = "Share local apps and folders with other devices — over the LAN or peer-to-peer"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Share a port (e.g. 3000) or a path (e.g. ./photos)
    Serve {
        /// What to share: a port something is listening on, or a file/folder
        target: String,
        /// Share name: becomes <name>.local on the LAN, <name>.localhost via open
        #[arg(long)]
        name: Option<String>,
        /// Port for the HTTPS edge (default: 443, falling back to 4433)
        #[arg(long)]
        port: Option<u16>,
        /// Don't expose anything on the LAN (ticket only)
        #[arg(long)]
        local_only: bool,
        /// Don't print the QR code
        #[arg(long)]
        no_qr: bool,
    },
    /// Open a ticket from another machine
    Open {
        /// Ticket from `lclhst serve`
        ticket: lclhst::ticket::Ticket,
        /// Port for the HTTPS edge (default: 443, falling back to 4433)
        #[arg(long)]
        port: Option<u16>,
        /// Bind loopback only; don't re-share on this machine's LAN
        #[arg(long)]
        local_only: bool,
        /// Don't print the QR code
        #[arg(long)]
        no_qr: bool,
    },
    /// Install this machine's lclhst CA into the system trust store
    Trust,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve {
            target,
            name,
            port,
            local_only,
            no_qr,
        } => {
            let target = lclhst::Target::parse(&target)?;
            let name = name.unwrap_or_else(|| target.default_name());
            let display_name = name.clone();
            let ca = lclhst::ca::Ca::load_or_create(&lclhst::ca::Ca::default_dir())?;
            let (tx, rx) = oneshot::channel();
            let task = tokio::spawn(lclhst::serve(target, name, port, local_only, ca, tx));
            if let Ok(info) = rx.await {
                eprintln!("ticket: {}", info.ticket);
                eprintln!("on another machine: lclhst open {}", info.ticket);
                if let Some(lan) = info.lan {
                    let by_name = url(&format!("{display_name}.local"), lan.port());
                    let by_ip = url(&lan.ip().to_string(), lan.port());
                    eprintln!("on this network:    {by_name} (or {by_ip})");
                    eprintln!(
                        "trust on a phone:   http{}/.lclhst/",
                        by_name.strip_prefix("https").unwrap_or(&by_name)
                    );
                    if !no_qr {
                        print_qr(&by_ip);
                    }
                }
                eprintln!("Ctrl-C to stop");
            }
            race_ctrl_c(task).await
        }
        Cmd::Open {
            ticket,
            port,
            local_only,
            no_qr,
        } => {
            let name = ticket.name.clone();
            let ca = lclhst::ca::Ca::load_or_create(&lclhst::ca::Ca::default_dir())?;
            let (tx, rx) = oneshot::channel();
            let task = tokio::spawn(lclhst::open(ticket, port, local_only, ca, tx));
            if let Ok(addr) = rx.await {
                eprintln!(
                    "this machine:    {}",
                    url(&format!("{name}.localhost"), addr.port())
                );
                if !local_only {
                    let by_name = url(&format!("{name}.local"), addr.port());
                    eprintln!("on this network: {by_name}");
                    eprintln!(
                        "trust on a phone: http{}/.lclhst/",
                        by_name.strip_prefix("https").unwrap_or(&by_name)
                    );
                    if !no_qr
                        && let Ok(ips) = lclhst::mdns::lan_ips()
                        && let Some(ip) = ips.first()
                    {
                        print_qr(&url(&ip.to_string(), addr.port()));
                    }
                }
                eprintln!("(run `lclhst trust` once on this machine for a clean padlock)");
            }
            race_ctrl_c(task).await
        }
        Cmd::Trust => {
            let dir = lclhst::ca::Ca::default_dir();
            // Ensure the CA exists before installing it.
            lclhst::ca::Ca::load_or_create(&dir)?;
            lclhst::trust::install(&dir.join("rootCA.pem"))
        }
    }
}

/// https URL with the port elided when it's the https default.
fn url(host: &str, port: u16) -> String {
    if port == 443 {
        format!("https://{host}")
    } else {
        format!("https://{host}:{port}")
    }
}

/// Render a QR code for phones to scan, on stderr, TTY only. The by-IP URL
/// goes in the QR: it works even when mDNS is blocked, and the leaf cert
/// carries the IP as a SAN so TLS validates once the CA is trusted.
fn print_qr(target: &str) {
    if !std::io::stderr().is_terminal() {
        return;
    }
    match fast_qr::QRBuilder::new(target).build() {
        Ok(qr) => {
            eprintln!("\nscan to open {target}:\n{}", qr.to_str());
        }
        Err(e) => tracing::debug!("QR generation failed: {e}"),
    }
}

async fn race_ctrl_c(task: tokio::task::JoinHandle<anyhow::Result<()>>) -> anyhow::Result<()> {
    tokio::select! {
        r = task => r?,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\nbye");
            Ok(())
        }
    }
}
