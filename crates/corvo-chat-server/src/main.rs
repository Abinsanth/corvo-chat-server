use std::{net::Ipv4Addr, path::PathBuf, sync::Arc};

use clap::{Args, Parser, Subcommand};
use corvo_chat_server::{
    AppState, Tls, admin, app, cleanup,
    clock::Clock,
    db::{self, Db},
    key,
    limits::Limits,
    serve,
};
use tokio::net::TcpListener;

/// The corvo-chat server: stores and forwards ciphertext only.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct DataDir {
    /// Where the server keeps its data (`server.db`, `opaque.key`, and the certificate cache in `acme/`).
    #[arg(long, default_value = "/var/lib/corvo-chat")]
    data_dir: PathBuf,
}

#[derive(Subcommand)]
enum Command {
    /// Serve HTTPS, getting and renewing the certificate from Let's Encrypt.
    Serve {
        /// The name the certificate is for.
        #[arg(long, default_value = "chat.corvo.live")]
        domain: String,
        #[command(flatten)]
        dir: DataDir,
        /// The port to listen on. Let's Encrypt checks the certificate on 443.
        #[arg(long, default_value_t = 443)]
        port: u16,
        /// How many devices one account may be signed in on.
        #[arg(long, default_value_t = 3)]
        max_devices: usize,
    },
    /// Make the OPAQUE server key (once).
    Keygen {
        #[command(flatten)]
        dir: DataDir,
    },
    /// Activate a pending account that has no inviter.
    Activate {
        /// The account's number.
        number: String,
        #[command(flatten)]
        dir: DataDir,
    },
    /// Make an invite code for a pending account.
    Invite {
        /// The account's number.
        number: String,
        #[command(flatten)]
        dir: DataDir,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Serve { domain, dir, port, max_devices } => {
            tracing_subscriber::fmt().with_ansi(false).with_max_level(tracing::Level::INFO).init();
            let data_dir = dir.data_dir;
            let setup = key::load(&data_dir)?;
            let conn = db::open(&data_dir.join(db::DB_FILE))?;
            let limits = Limits { max_devices, ..Limits::default() };
            let state = AppState {
                db: Db::new(conn),
                setup: Arc::new(setup),
                clock: Clock::system(),
                limits: Arc::new(limits),
            };
            let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await?;
            tracing::info!("corvo-chat-server {} listening on :{port} for {domain}", env!("CARGO_PKG_VERSION"));
            cleanup::spawn_cleanup(state.clone());
            serve(listener, Tls::Acme { domain, cache_dir: data_dir.join("acme") }, app(state)).await;
            Ok(())
        }
        Command::Keygen { dir } => {
            key::generate(&dir.data_dir)?;
            println!("opaque.key written. Back it up offline: losing it breaks every account.");
            Ok(())
        }
        Command::Activate { number, dir } => {
            println!("{}", admin::activate(&dir.data_dir, &number)?);
            Ok(())
        }
        Command::Invite { number, dir } => {
            println!("{}", admin::invite(&dir.data_dir, &number)?);
            Ok(())
        }
    }
}
