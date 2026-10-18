mod common;

use common::{
    chat::{contacts, first_message},
    client::{Client, assert_error, start_server},
    person::{Person, device},
};
use corvo_chat_proto::{
    Blob,
    chat::{EventBody, Pin},
    frames::{ClientFrame, DeviceInfo, ErrorCode},
};

/// Events until one matches, skipping the rest.
async fn wait_for(client: &mut Client, mut matches: impl FnMut(&EventBody) -> bool) -> EventBody {
    loop {
        let event = client.next_event().await.event;
        if matches(&event) {
            return event;
        }
    }
}

async fn next_devices(client: &mut Client) -> Vec<DeviceInfo> {
    match wait_for(client, |e| matches!(e, EventBody::Devices { .. })).await {
        EventBody::Devices { devices } => devices,
        _ => unreachable!(),
    }
}

async fn next_pin(client: &mut Client) -> Pin {
    match wait_for(client, |e| matches!(e, EventBody::Pin { .. })).await {
        EventBody::Pin { pin } => pin,
        _ => unreachable!(),
    }
}

/// A second device of `person`, signed in and synced.
async fn second_device(server: &common::client::TestServer, person: &Person) -> Client {
    let (mut client, _) = person.sign_in(server, &device("phone")).await;
    client.sync_from(Some(0)).await;
    client
}

#[tokio::test]
async fn display_name_change_reaches_my_devices_and_contacts_only() {
    let server = start_server().await;
    let (abin, mut a, _rahul, mut b) = contacts(&server).await;
    let (sam, mut s) = Person::active(&server, "sam").await;
    s.sync_from(Some(0)).await;
    first_message(&mut s, &sam, abin.number).await;
    let mut a2 = second_device(&server, &abin).await;
    s.drain().await;

    a.send(&ClientFrame::SetDisplayName { display_name: "  Abin P ".into() }).await;
    assert_eq!(b.next_person(abin.number).await.display_name.as_deref(), Some("Abin P"));
    for client in [&mut a, &mut a2] {
        let profile = wait_for(client, |e| matches!(e, EventBody::Profile { .. })).await;
        assert!(matches!(profile, EventBody::Profile { account } if account.display_name == "Abin P"));
    }
    assert!(s.quiet().await, "Sam only asked; Abin hasn't shared his name with him");
}

#[tokio::test]
async fn bad_names_are_refused() {
    let server = start_server().await;
    let (_abin, mut a) = Person::active(&server, "abin").await;
    a.send(&ClientFrame::SetDisplayName { display_name: "".into() }).await;
    assert_error(a.recv().await, ErrorCode::BadDisplayName);
    a.send(&ClientFrame::RenameDevice { name: "a\u{7}".into() }).await;
    assert_error(a.recv().await, ErrorCode::BadDevice);
}

#[tokio::test]
async fn renaming_this_device_shows_in_devices_events_on_all_devices() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    a.sync_from(Some(0)).await;
    let mut a2 = second_device(&server, &abin).await;
    a.drain().await;
    a2.drain().await;
    a2.send(&ClientFrame::RenameDevice { name: "work phone".into() }).await;
    let theirs = next_devices(&mut a).await;
    let renamed = theirs.iter().find(|d| d.name == "work phone").expect("the renamed device");
    assert!(!renamed.this_device);
    let mine = next_devices(&mut a2).await;
    assert!(mine.iter().any(|d| d.name == "work phone" && d.this_device));
}

#[tokio::test]
async fn signing_in_a_new_device_sends_a_devices_event() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    a.sync_from(Some(0)).await;
    abin.sign_in(&server, &device("phone")).await;
    let devices = next_devices(&mut a).await;
    assert_eq!(devices.len(), 2);
    assert_eq!(
        devices.iter().filter(|d| d.this_device).map(|d| d.id.clone()).collect::<Vec<_>>(),
        vec![abin.device.id]
    );
}

#[tokio::test]
async fn signing_out_a_device_sends_a_devices_event() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    a.sync_from(Some(0)).await;
    let mut a2 = second_device(&server, &abin).await;
    a.drain().await;
    a2.send(&ClientFrame::SignOut).await;
    assert_eq!(next_devices(&mut a).await.len(), 1);
}

#[tokio::test]
async fn pins_sync_to_the_other_device_and_into_the_snapshot() {
    let server = start_server().await;
    let (abin, mut a, rahul, _b) = contacts(&server).await;
    let mut a2 = second_device(&server, &abin).await;
    let fresh_device = device("fresh");
    for pin in [Some(vec![1, 2]), Some(vec![3]), None] {
        let pin = Pin { number: rahul.number, pin: pin.map(Blob) };
        a.send(&ClientFrame::SetPin { pin: pin.clone() }).await;
        assert_eq!(next_pin(&mut a2).await, pin);
        let (mut fresh, _) = abin.sign_in(&server, &fresh_device).await;
        let pins = fresh.sync_from(None).await.snapshot.expect("a snapshot").pins;
        assert_eq!(pins, pin.pin.is_some().then(|| pin.clone()).into_iter().collect::<Vec<_>>());
    }
}
