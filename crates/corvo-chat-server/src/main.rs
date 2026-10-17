use std::{net::Ipv4Addr, path::PathBuf};

use clap::{Parser, Subcommand};
use corvo_chat_server::{Tls, app, serve};
use tokio::net::TcpListener;

/// The corvo-chat server: stores and forwards ciphertext only.
#[derive(Parser)]
#[command(version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve HTTPS, getting and renewing the certificate from Let's Encrypt.
    Serve {
        /// The name the certificate is for.
        #[arg(long, default_value = "chat.corvo.live")]
        domain: String,
        /// Where the server keeps its data (the certificate cache is in `acme/`).
        #[arg(long, default_value = "/var/lib/corvo-chat")]
        data_dir: PathBuf,
        /// The port to listen on. Let's Encrypt checks the certificate on 443.
        #[arg(long, default_value_t = 443)]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Args::parse().command {
        Command::Serve { domain, data_dir, port } => {
            tracing_subscriber::fmt().with_ansi(false).with_max_level(tracing::Level::INFO).init();
            let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await?;
            tracing::info!("corvo-chat-server {} listening on :{port} for {domain}", env!("CARGO_PKG_VERSION"));
            serve(listener, Tls::Acme { domain, cache_dir: data_dir.join("acme") }, app()).await;
            Ok(())
        }
    }
}
