//! A test person: signs up over a real connection with real OPAQUE, and
//! sends stand-in random blobs for the locked copies.

use corvo_chat_proto::{
    B64, Blob, Number, PublicKeys, exchange_key_proof,
    frames::{ClientFrame, Device, DeviceKind, ServerFrame},
};
use corvo_chat_server::accounts;
use ed25519_dalek::{Signer, SigningKey};

use super::client::{Client, TestServer, register_finish, register_start};

/// A fresh device with a random id.
pub fn device(name: &str) -> Device {
    Device { id: B64(rand::random()), kind: DeviceKind::Cli, name: name.into() }
}

pub struct Person {
    pub username: String,
    pub number: Number,
    pub password: String,
    pub recovery: String,
    pub device: Device,
    pub token: B64<32>,
    pub keys: SigningKey,
}

/// A signup that has had its `signup_offer`: the `signup_finish` frame is
/// ready to send (or to tamper with first).
#[derive(Debug)]
pub struct Begun {
    pub username: String,
    pub number: Number,
    pub password: String,
    pub recovery: String,
    pub device: Device,
    pub keys: SigningKey,
    pub finish: ClientFrame,
}

/// Sends `signup_start` on a client that has said hello. An error frame
/// comes back as `Err`.
pub async fn begin_signup(
    client: &mut Client,
    base: &str,
    display_name: &str,
    device: Device,
) -> Result<Begun, ServerFrame> {
    let password: String = format!("pw-{:016x}", rand::random::<u64>());
    let recovery: String = format!("rc-{:016x}", rand::random::<u64>());
    let (pw_state, pw_request) = register_start(&password);
    let (rc_state, rc_request) = register_start(&recovery);
    client
        .send(&ClientFrame::SignupStart {
            base: base.into(),
            display_name: display_name.into(),
            device: device.clone(),
            password: pw_request,
            recovery: rc_request,
        })
        .await;
    let (username, number, pw_response, rc_response) = match client.recv().await {
        ServerFrame::SignupOffer { username, number, password, recovery } => (username, number, password, recovery),
        other => return Err(other),
    };
    let (pw_upload, _) = register_finish(pw_state, &password, pw_response);
    let (rc_upload, _) = register_finish(rc_state, &recovery, rc_response);
    let keys = SigningKey::from_bytes(&rand::random());
    let exchange: [u8; 32] = rand::random();
    let signature = keys.sign(&exchange_key_proof(&exchange)).to_bytes();
    let finish = ClientFrame::SignupFinish {
        password: pw_upload,
        recovery: rc_upload,
        password_copy: Blob(rand::random::<[u8; 200]>().to_vec()),
        recovery_copy: Blob(rand::random::<[u8; 200]>().to_vec()),
        public_keys: PublicKeys {
            signing: B64(keys.verifying_key().to_bytes()),
            exchange: B64(exchange),
            signature: B64(signature),
        },
    };
    Ok(Begun { username, number, password, recovery, device, keys, finish })
}

impl Person {
    /// Signs up and keeps the signup connection open (pending).
    pub async fn sign_up(server: &TestServer, base: &str) -> (Person, Client) {
        match Person::try_sign_up(server, base, device("laptop")).await {
            Ok(done) => done,
            Err(frame) => panic!("signup failed: {frame:?}"),
        }
    }

    /// Like `sign_up` on a chosen device; the refusal comes back as `Err`.
    pub async fn try_sign_up(server: &TestServer, base: &str, device: Device) -> Result<(Person, Client), ServerFrame> {
        let mut client = Client::connect(server).await;
        client.hello().await;
        let begun = begin_signup(&mut client, base, "Abin", device).await?;
        client.send(&begun.finish).await;
        let ServerFrame::SignedIn { token, .. } = client.recv().await else {
            panic!("expected signed_in");
        };
        let person = Person {
            username: begun.username,
            number: begun.number,
            password: begun.password,
            recovery: begun.recovery,
            device: begun.device,
            token,
            keys: begun.keys,
        };
        Ok((person, client))
    }

    /// Signs up, then activates through the database, as an admin would.
    pub async fn active(server: &TestServer, base: &str) -> (Person, Client) {
        let (person, client) = Person::sign_up(server, base).await;
        let name = person.username.clone();
        let now = server.state.clock.now();
        server
            .state
            .db
            .call(move |c| {
                let id = accounts::by_username(c, &name, now).unwrap().expect("the account exists").id;
                accounts::activate(c, id, None, now).unwrap();
            })
            .await;
        (person, client)
    }

    /// The account id.
    pub async fn id(&self, server: &TestServer) -> i64 {
        let (name, now) = (self.username.clone(), server.state.clock.now());
        server.state.db.call(move |c| accounts::by_username(c, &name, now).unwrap().expect("the account").id).await
    }
}
