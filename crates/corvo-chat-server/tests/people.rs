mod common;

use common::{
    chat::{contacts, first_message, strangers},
    client::{Client, TestServer, assert_error, start_server},
    person::{Person, device},
};
use corvo_chat_proto::{
    Number,
    chat::{EventBody, Marks, Relation},
    frames::{ClientFrame, ErrorCode, ServerFrame},
};
use corvo_chat_server::accounts;

async fn act(client: &mut Client, frame: ClientFrame) {
    client.send(&frame).await;
}

/// Events until one matches, skipping the rest.
async fn wait_for(client: &mut Client, mut matches: impl FnMut(&EventBody) -> bool) -> EventBody {
    loop {
        let event = client.next_event().await.event;
        if matches(&event) {
            return event;
        }
    }
}

/// The seqs of the messages a fresh device of `person` gets in its snapshot.
async fn snapshot_seqs(server: &TestServer, person: &Person) -> Vec<i64> {
    let (mut fresh, _) = person.sign_in(server, &device("fresh")).await;
    let synced = fresh.sync_from(None).await;
    synced.histories.iter().flat_map(|(_, messages, _)| messages.iter().map(|m| m.seq)).collect()
}

async fn relation_rows(server: &TestServer, person: &Person) -> i64 {
    let id = person.id(server).await;
    server
        .state
        .db
        .call(move |c| {
            c.query_row("SELECT COUNT(*) FROM relations WHERE account_id = ?1 OR other_id = ?1", [id], |r| r.get(0))
                .unwrap()
        })
        .await
}

fn accept(number: Number) -> ClientFrame {
    ClientFrame::Accept { number }
}

#[tokio::test]
async fn accept_makes_contacts_both_ways() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = strangers(&server).await;
    first_message(&mut a, &abin, rahul.number).await;
    act(&mut b, accept(abin.number)).await;
    assert_eq!(b.next_person(abin.number).await.relation, Relation::Request, "the request came first");
    assert_eq!(b.next_person(abin.number).await.relation, Relation::Contact);
    assert_eq!(a.next_person(rahul.number).await.relation, Relation::Asked);
    let now_contact = a.next_person(rahul.number).await;
    assert_eq!((now_contact.relation, now_contact.display_name.as_deref()), (Relation::Contact, Some("Abin")));
}

#[tokio::test]
async fn request_marks_appear_only_on_accept() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = strangers(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    a.drain().await;
    b.send(&ClientFrame::Received { chat_id: chat_id.clone(), through: 1 }).await;
    b.send(&ClientFrame::Read { chat_id, through: 1 }).await;
    assert!(a.quiet().await, "a request shows no marks");
    act(&mut b, accept(abin.number)).await;
    let marks = wait_for(&mut a, |e| matches!(e, EventBody::Marks { .. })).await;
    assert!(matches!(marks, EventBody::Marks { marks: Marks { received_through: 1, read_through: 1 }, .. }));
}

#[tokio::test]
async fn reject_hides_the_request_and_the_sender_is_not_told() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = strangers(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    a.drain().await;
    b.drain().await;
    act(&mut b, ClientFrame::Reject { number: abin.number }).await;
    wait_for(&mut b, |e| matches!(e, EventBody::Cleared { cleared_through: 1, .. })).await;
    assert_eq!(b.next_person(abin.number).await.relation, Relation::None);
    assert!(a.quiet().await, "the sender isn't told");

    assert!(matches!(a.send_body(&chat_id, b"again".to_vec(), None).await, ServerFrame::Sent { seq: 2, .. }));
    assert_eq!(b.next_person(abin.number).await.relation, Relation::Request);
    assert_eq!(snapshot_seqs(&server, &rahul).await, vec![2]);
}

#[tokio::test]
async fn block_refuses_silently_and_unblock_makes_a_stranger() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = strangers(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    b.drain().await;
    act(&mut b, ClientFrame::Block { number: abin.number }).await;
    assert_eq!(b.next_person(abin.number).await.relation, Relation::Blocked);
    assert!(matches!(a.send_body(&chat_id, b"x".to_vec(), None).await, ServerFrame::Sent { .. }));
    assert!(b.quiet().await);

    act(&mut b, ClientFrame::Unblock { number: abin.number }).await;
    assert_eq!(b.next_person(abin.number).await.relation, Relation::None);
    assert!(matches!(a.send_body(&chat_id, b"y".to_vec(), None).await, ServerFrame::Sent { seq: 3, .. }));
    assert_eq!(b.next_person(abin.number).await.relation, Relation::Request);
    let message = wait_for(&mut b, |e| matches!(e, EventBody::Message { .. })).await;
    assert!(matches!(message, EventBody::Message { message } if message.seq == 3));
}

#[tokio::test]
async fn remove_without_history_keeps_it_and_a_new_message_is_a_request() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = contacts(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    b.drain().await;
    act(&mut b, ClientFrame::Remove { number: abin.number, clear_history: false }).await;
    assert_eq!(b.next_person(abin.number).await.relation, Relation::None);
    assert_eq!(snapshot_seqs(&server, &rahul).await, vec![1], "the history stays");
    a.send_body(&chat_id, b"x".to_vec(), None).await;
    assert_eq!(b.next_person(abin.number).await.relation, Relation::Request);
}

#[tokio::test]
async fn remove_with_history_clears_my_side_only() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = contacts(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    for _ in 0..2 {
        a.send_body(&chat_id, b"x".to_vec(), None).await;
    }
    b.drain().await;
    act(&mut b, ClientFrame::Remove { number: abin.number, clear_history: true }).await;
    wait_for(&mut b, |e| matches!(e, EventBody::Cleared { cleared_through: 3, .. })).await;
    assert_eq!(snapshot_seqs(&server, &rahul).await, Vec::<i64>::new());
    assert_eq!(snapshot_seqs(&server, &abin).await, vec![1, 2, 3]);
}

#[tokio::test]
async fn accepting_without_a_request_is_not_allowed() {
    let server = start_server().await;
    let (abin, _a, _rahul, mut b) = strangers(&server).await;
    for frame in [
        accept(abin.number),
        ClientFrame::Reject { number: abin.number },
        ClientFrame::Unblock { number: abin.number },
        ClientFrame::Remove { number: abin.number, clear_history: false },
    ] {
        act(&mut b, frame).await;
        assert_error(b.recv().await, ErrorCode::NotAllowed);
    }
    act(&mut b, accept(Number::from_body(999_999_999).unwrap())).await;
    assert_error(b.recv().await, ErrorCode::NumberUnknown);
}

#[tokio::test]
async fn invite_activation_makes_contacts() {
    let server = start_server().await;
    let (abin, mut a) = Person::active(&server, "abin").await;
    a.sync_from(Some(0)).await;
    let (sam, mut s) = Person::sign_up(&server, "sam").await;
    a.send(&ClientFrame::Invite { number: sam.number }).await;
    let ServerFrame::Invited { invite } = a.recv().await else { panic!("expected invited") };
    s.send(&ClientFrame::Activate { code: invite.code }).await;
    assert!(matches!(s.recv().await, ServerFrame::Activated { .. }));
    let person = a.next_person(sam.number).await;
    assert_eq!((person.relation, person.display_name.as_deref()), (Relation::Contact, Some("Abin")));
    let synced = s.sync_from(Some(0)).await;
    assert!(synced.events.iter().any(|e| matches!(&e.event,
        EventBody::Person { person } if person.number == abin.number && person.relation == Relation::Contact)));

    // The admin's activation makes no contacts.
    let (lone, _l) = Person::sign_up(&server, "lone").await;
    let (id, now) = (lone.id(&server).await, server.state.clock.now());
    server.state.db.call(move |c| accounts::activate(c, id, None, now).unwrap()).await;
    assert_eq!(relation_rows(&server, &lone).await, 0);
}

#[tokio::test]
async fn two_people_asking_each_other_become_contacts() {
    let server = start_server().await;
    let (abin, mut a, rahul, mut b) = strangers(&server).await;
    let chat_id = first_message(&mut a, &abin, rahul.number).await;
    act(&mut b, ClientFrame::Reject { number: abin.number }).await;
    wait_for(&mut b, |e| matches!(e, EventBody::Cleared { .. })).await;
    a.drain().await;
    b.drain().await;
    assert!(matches!(b.send_body(&chat_id, b"hi".to_vec(), None).await, ServerFrame::Sent { .. }));
    // Events show the current state, so even Rahul's "asked" reads as contact.
    assert_eq!(a.next_person(rahul.number).await.relation, Relation::Contact);
    assert_eq!(b.next_person(abin.number).await.relation, Relation::Contact);
}
