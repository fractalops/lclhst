use clap::{Parser, Subcommand};
use tokio::sync::oneshot;

#[derive(Parser)]
#[command(
    name = "lclhst",
    version,
    about = "Share a running localhost app peer-to-peer"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Share a local port; prints a ticket for the other side
    Serve {
        /// Local port the app is running on
        port: u16,
        /// Tunnel name: becomes <name>.localhost on the other side
        #[arg(long, default_value = "app")]
        name: String,
    },
    /// Open a ticket; serves it at https://<name>.localhost:<port>
    Open {
        /// Ticket from `lclhst serve`
        ticket: lclhst::ticket::Ticket,
        /// Local port for the HTTPS edge
        #[arg(long, default_value_t = 4433)]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve { port, name } => {
            let (tx, rx) = oneshot::channel();
            let task = tokio::spawn(lclhst::serve(port, name, tx));
            if let Ok(ticket) = rx.await {
                eprintln!("ticket: {ticket}");
                eprintln!("on the other machine: lclhst open {ticket}");
                eprintln!("waiting for peers — Ctrl-C to stop");
            }
            race_ctrl_c(task).await
        }
        Cmd::Open { ticket, port } => {
            let name = ticket.name.clone();
            let (tx, rx) = oneshot::channel();
            let task = tokio::spawn(lclhst::open(ticket, port, tx));
            if let Ok(addr) = rx.await {
                eprintln!("serving {name} → https://{name}.localhost:{}", addr.port());
                eprintln!("(self-signed cert in v0.1 — your browser will warn; curl -k works)");
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
