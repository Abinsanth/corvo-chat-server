mod common;

use std::time::Duration;

use common::client::{Client, start_server};
use corvo_chat_proto::frames::{ClientFrame, ErrorCode, ServerFrame};
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
async fn anonymous_connection_is_closed_after_five_minutes() {
    let server = start_server().await;
    let mut client = Client::connect(&server).await;
    client.hello().await;
    // Let the server reach its loop in real time; a paused clock could jump
    // ahead of it.
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::pause();
    assert!(!client.closed_within(Duration::from_secs(299)).await, "closed before 299 s");
    // Step to 301 s by hand, then wait for the close in real time: left to
    // itself, the paused clock can pass 301 s before the bytes arrive.
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    assert!(client.closed_within(Duration::from_millis(500)).await, "still open at 301 s");
}
