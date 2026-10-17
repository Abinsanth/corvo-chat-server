//! Its own test binary: it lowers the whole process's open-file limit.
#![cfg(unix)]

mod common;

use std::time::Duration;

use common::{connect, fetch, self_signed};
use corvo_chat_server::{Tls, app, serve};
use rlimit::Resource;
use tokio::{
    net::{TcpListener, TcpStream},
    time::{sleep, timeout},
};

#[tokio::test]
async fn running_out_of_file_handles_does_not_stop_the_server() {
    let (config, der) = self_signed();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a free port");
    let addr = listener.local_addr().expect("a bound address");
    let server = tokio::spawn(serve(listener, Tls::Static(config), app()));

    let (soft, hard) = Resource::NOFILE.get().expect("the current limit");
    Resource::NOFILE.set(64, hard).expect("a lower limit");
    let mut held = Vec::new();
    while let Ok(Ok(tcp)) = timeout(Duration::from_secs(1), TcpStream::connect(addr)).await {
        held.push(tcp);
        assert!(held.len() < 1000, "the lower limit didn't take");
    }
    // The server now meets a full file table on accept.
    sleep(Duration::from_millis(300)).await;
    drop(held);
    Resource::NOFILE.set(soft, hard).expect("the old limit back");
    sleep(Duration::from_millis(300)).await;

    assert!(!server.is_finished(), "serve stopped");
    let response = fetch(&mut connect(addr, der).await, "/").await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
}
