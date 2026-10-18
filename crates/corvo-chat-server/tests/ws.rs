mod common;

use std::time::Duration;

use common::client::{Client, start_server, start_server_with};
use corvo_chat_proto::frames::{ClientFrame, ErrorCode, ServerFrame};
use corvo_chat_server::{limits::Limits, ws::SEND_TIMEOUT};
use tokio::{net::TcpSocket, time::timeout};
use tokio_tungstenite::tungstenite::Message;

/// Asserts `frame` is an error with `code`.
fn assert_error(frame: ServerFrame, code: ErrorCode) {
    match frame {
        ServerFrame::Error { code: got, .. } => assert_eq!(got, code),
        other => panic!("expected {code:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn hello_gets_ready() {
    let server = start_server().await;
    let mut client = Client::connect(&server).await;
    client
        .send(&ClientFrame::Hello { protocol_version: corvo_chat_proto::PROTOCOL_VERSION, app_version: "t".into() })
        .await;
    match client.recv().await {
        ServerFrame::Ready { server_version } => assert_eq!(server_version, env!("CARGO_PKG_VERSION")),
        other => panic!("expected ready, got {other:?}"),
    }
}

#[tokio::test]
async fn old_protocol_version_is_told_to_update() {
    let server = start_server().await;
    let mut client = Client::connect(&server).await;
    client.send(&ClientFrame::Hello { protocol_version: 0, app_version: "t".into() }).await;
    match client.recv().await {
        ServerFrame::Error { code: ErrorCode::UpdateRequired, message, .. } => assert_eq!(message, "run /update"),
        other => panic!("expected update_required, got {other:?}"),
    }
    assert!(client.closed().await);
}

#[tokio::test]
async fn first_frame_must_be_hello() {
    let server = start_server().await;
    let mut client = Client::connect(&server).await;
    client.send(&ClientFrame::ListDevices).await;
    assert_error(client.recv().await, ErrorCode::BadFrame);
    assert!(client.closed().await);
}

#[tokio::test]
async fn garbage_frame_gets_bad_frame_and_the_connection_stays_open() {
    let server = start_server().await;
    let mut client = Client::connect(&server).await;
    client.hello().await;
    client.send_raw(Message::text("{")).await;
    assert_error(client.recv().await, ErrorCode::BadFrame);
    client.send_raw(Message::binary(vec![1, 2, 3])).await;
    assert_error(client.recv().await, ErrorCode::BadFrame);
    client.send(&ClientFrame::ListDevices).await;
    assert_error(client.recv().await, ErrorCode::NotSignedIn);
}

#[tokio::test]
async fn anonymous_connection_is_closed_after_its_timeout() {
    assert_eq!(Limits::default().anon_timeout_secs, 300);
    let server = start_server_with(Limits { anon_timeout_secs: 1, ..Limits::default() }).await;
    let mut client = Client::connect(&server).await;
    client.hello().await;
    assert!(!client.closed_within(Duration::from_millis(500)).await, "closed before 500 ms");
    assert!(client.closed_within(Duration::from_secs(3)).await, "still open at 3 s");
}

#[tokio::test]
async fn client_that_never_reads_is_closed_anyway() {
    let server = start_server().await;
    let socket = TcpSocket::new_v4().expect("a socket");
    socket.set_recv_buffer_size(4096).expect("a small receive buffer");
    let tcp = socket.connect(server.addr).await.expect("the server accepts");
    let mut client = Client::connect_over(&server, tcp).await;
    client.hello().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::pause();
    // Bad frames, each answered, never read: the server's replies fill both
    // buffers until its writes block. The paused clock moves only once every
    // task waits, so the server's deadlines are what end this.
    let spam = tokio::spawn(async move { while client.try_send_raw(Message::text("{")).await.is_ok() {} });
    let deadline = Duration::from_secs(Limits::default().anon_timeout_secs) + 2 * SEND_TIMEOUT + Duration::from_secs(1);
    timeout(deadline, spam).await.expect("closed by its deadlines").expect("the client task ends");
}
