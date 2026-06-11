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
        /// Port for the LAN HTTPS edge
        #[arg(long, default_value_t = 4433)]
        port: u16,
        /// Don't expose anything on the LAN (ticket only)
        #[arg(long)]
        local_only: bool,
    },
    /// Open a ticket from another machine
    Open {
        /// Ticket from `lclhst serve`
        ticket: lclhst::ticket::Ticket,
        /// Local port for the HTTPS edge
        #[arg(long, default_value_t = 4433)]
        port: u16,
        /// Bind loopback only; don't re-share on this machine's LAN
        #[arg(long)]
        local_only: bool,
    },
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
        } => {
            let target = lclhst::Target::parse(&target)?;
            let name = name.unwrap_or_else(|| target.default_name());
            let display_name = name.clone();
            let (tx, rx) = oneshot::channel();
            let task = tokio::spawn(lclhst::serve(target, name, port, local_only, tx));
            if let Ok(info) = rx.await {
                eprintln!("ticket: {}", info.ticket);
                eprintln!("on another machine: lclhst open {}", info.ticket);
                if let Some(lan) = info.lan {
                    eprintln!(
                        "on this network:    https://{display_name}.local:{} (or https://{lan})",
                        lan.port()
                    );
                }
                eprintln!("Ctrl-C to stop");
            }
            race_ctrl_c(task).await
        }
        Cmd::Open {
            ticket,
            port,
            local_only,
        } => {
            let name = ticket.name.clone();
            let (tx, rx) = oneshot::channel();
            let task = tokio::spawn(lclhst::open(ticket, port, local_only, tx));
            if let Ok(addr) = rx.await {
                eprintln!("this machine:    https://{name}.localhost:{}", addr.port());
                if !local_only {
                    eprintln!("on this network: https://{name}.local:{}", addr.port());
                }
                eprintln!("(self-signed cert — your browser will warn; curl -k works)");
            }
            race_ctrl_c(task).await
        }
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
