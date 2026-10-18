mod common;

use common::{
    chat::{contacts, first_message, strangers},
    client::{Client, start_server},
    person::device,
};
use corvo_chat_proto::frames::ClientFrame;
use tokio_tungstenite::tungstenite::Message;

/// Closes a connection and waits until the server has let go of it.
async fn close(mut client: Client) {
    client.send_raw(Message::Close(None)).await;
    assert!(client.closed().await);
}

#[tokio::test]
async fn contact_coming_online_and_leaving_is_pushed() {
    let server = start_server().await;
    let (abin, a, _rahul, mut b) = contacts(&server).await;
    let (a2, _) = abin.sign_in(&server, &device("phone")).await;
    assert!(b.quiet().await, "Abin was already online");
    close(a2).await;
    assert!(b.quiet().await, "Abin is still online on the laptop");
    close(a).await;
    assert_eq!(b.next_presence().await, (abin.number, false));
    let _a = abin.client_for(&server).await;
    assert_eq!(b.next_presence().await, (abin.number, true));
}

#[tokio::test]
async fn sync_lists_online_contacts() {
    let server = start_server().await;
    let (abin, _a, rahul, _b) = contacts(&server).await;
    let (mut phone, _) = rahul.sign_in(&server, &device("phone")).await;
    phone.sync_from(Some(0)).await;
    assert_eq!(phone.next_presence().await, (abin.number, true));
}

#[tokio::test]
async fn requests_and_strangers_see_no_presence() {
    let server = start_server().await;
    let (abin, mut a, rahul, b) = strangers(&server).await;
    first_message(&mut a, &abin, rahul.number).await;
    a.drain().await;
    close(b).await;
    let _b = rahul.client_for(&server).await;
    assert!(a.quiet().await);
}

#[tokio::test]
async fn blocking_shows_you_offline_to_them() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = contacts(&server).await;
    b.send(&ClientFrame::Block { number: abin.number }).await;
    assert_eq!(a.next_presence().await, (rahul.number, false));
    close(b).await;
    let _b = rahul.client_for(&server).await;
    assert!(a.quiet().await, "a blocked contact hears nothing more");
}

#[tokio::test]
async fn same_device_signing_in_again_never_shows_offline() {
    let server = start_server().await;
    let (abin, a, _rahul, mut b) = contacts(&server).await;
    let (_again, _) = abin.sign_in(&server, &abin.device).await;
    let mut a = a;
    assert!(a.closed_quietly().await, "the old connection is replaced");
    assert!(b.quiet().await);
}
