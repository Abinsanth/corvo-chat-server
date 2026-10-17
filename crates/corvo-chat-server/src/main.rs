use clap::Parser;

/// The corvo-chat server: stores and forwards ciphertext only.
#[derive(Parser)]
#[command(version)]
struct Args {}

fn main() {
    let _args = Args::parse();
    println!("corvo-chat-server {} has nothing to serve yet.", env!("CARGO_PKG_VERSION"));
}
