//! A test person: signs up over a real connection with real OPAQUE, and
//! sends stand-in random blobs for the locked copies.

use corvo_chat_proto::{
    B64, Blob, Number, PublicKeys, exchange_key_proof,
    frames::{ClientFrame, Device, DeviceKind, ServerFrame},
    opaque::{CredFinal, CredRequest, RegUpload, recovery_ksf},
};
use corvo_chat_server::accounts;
use ed25519_dalek::{Signer, SigningKey};

use super::client::{Client, Ksf, TestServer, login_finish_with, login_start, register_finish_with, register_start};

/// A fresh device with a random id.
pub fn device(name: &str) -> Device {
    Device { id: B64(rand::random()), kind: DeviceKind::Cli, name: name.into() }
}

/// A fresh random secret, for a password or a recovery code.
pub fn secret(prefix: &str) -> String {
    format!("{prefix}-{:016x}", rand::random::<u64>())
}

/// 200 random bytes standing in for a locked copy.
pub fn copy() -> Vec<u8> {
    rand::random::<[u8; 200]>().to_vec()
}

pub struct Person {
    pub username: String,
    pub number: Number,
    pub password: String,
    pub recovery: String,
    pub device: Device,
    pub token: B64<32>,
    pub keys: SigningKey,
    pub exchange: [u8; 32],
    pub password_copy: Vec<u8>,
    pub recovery_copy: Vec<u8>,
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
    pub exchange: [u8; 32],
    pub finish: ClientFrame,
}

/// Sends `signup_start` on a client that has said hello. An error frame
/// comes back as `Err`.
pub async fn begin_signup(
    client: &mut Client,
    base: &str,
    display_name: &str,
    device: Device,
) -> Result<Begun, Box<ServerFrame>> {
    begin_signup_with(&recovery_ksf(), client, base, display_name, device).await
}

/// Like `begin_signup`, stretching the password with `ksf`.
pub async fn begin_signup_with(
    ksf: &Ksf,
    client: &mut Client,
    base: &str,
    display_name: &str,
    device: Device,
) -> Result<Begun, Box<ServerFrame>> {
    let password = secret("pw");
    let recovery = secret("rc");
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
        other => return Err(Box::new(other)),
    };
    let (pw_upload, _) = register_finish_with(ksf, pw_state, &password, pw_response);
    let (rc_upload, _) = register_finish_with(&recovery_ksf(), rc_state, &recovery, rc_response);
    let keys = SigningKey::from_bytes(&rand::random());
    let exchange: [u8; 32] = rand::random();
    let signature = keys.sign(&exchange_key_proof(&exchange)).to_bytes();
    let finish = ClientFrame::SignupFinish {
        password: pw_upload,
        recovery: rc_upload,
        password_copy: Blob(copy()),
        recovery_copy: Blob(copy()),
        public_keys: PublicKeys {
            signing: B64(keys.verifying_key().to_bytes()),
            exchange: B64(exchange),
            signature: B64(signature),
        },
    };
    Ok(Begun { username, number, password, recovery, device, keys, exchange, finish })
}

/// How the client side of an OPAQUE login (sign-in, recovery or reauth) went.
#[derive(Debug)]
#[expect(clippy::large_enum_variant, reason = "tests match the frame inside by value; a box would stop that")]
pub enum Outcome {
    /// The server's last answer: to the finish, or its refusal of the start.
    Server(ServerFrame),
    /// The secret didn't match, which only the client sees, so no finish was sent.
    ClientRefused,
}

/// Sends `start` with a credential request and, if the secret checks out
/// against the offer, `finish` with the finalization.
pub async fn prove_with(
    ksf: &Ksf,
    client: &mut Client,
    secret: &str,
    start: impl FnOnce(CredRequest) -> ClientFrame,
    finish: impl FnOnce(CredFinal) -> ClientFrame,
) -> Outcome {
    let (state, request) = login_start(secret);
    client.send(&start(request)).await;
    let response = match client.recv().await {
        ServerFrame::LoginOffer { response } => response,
        other => return Outcome::Server(other),
    };
    let Some((finalization, _)) = login_finish_with(ksf, state, secret, response) else {
        return Outcome::ClientRefused;
    };
    client.send(&finish(finalization)).await;
    Outcome::Server(client.recv().await)
}

/// `login_start` and `login_finish`.
pub async fn login(client: &mut Client, username: &str, password: &str, device: &Device) -> Outcome {
    let start = |request| ClientFrame::LoginStart { username: username.into(), request, device: device.clone() };
    prove_with(&recovery_ksf(), client, password, start, |finalization| ClientFrame::LoginFinish { finalization }).await
}

/// `recovery_start` and `recovery_finish`.
pub async fn recover(client: &mut Client, username: &str, code: &str, device: &Device) -> Outcome {
    let start = |request| ClientFrame::RecoveryStart { username: username.into(), request, device: device.clone() };
    prove_with(&recovery_ksf(), client, code, start, |finalization| ClientFrame::RecoveryFinish { finalization }).await
}

/// `reauth_start` and `reauth_finish` on a signed-in client.
pub async fn reauth(client: &mut Client, password: &str) -> Outcome {
    let start = |request| ClientFrame::ReauthStart { request };
    prove_with(&recovery_ksf(), client, password, start, |finalization| ClientFrame::ReauthFinish { finalization })
        .await
}

/// Sends `reregister` for the given new secrets and finishes the
/// registrations it gets back: the uploads, in the same order.
pub async fn reregister(
    client: &mut Client,
    password: Option<&str>,
    recovery: Option<&str>,
) -> (Option<RegUpload>, Option<RegUpload>) {
    let pw = password.map(|secret| (secret, register_start(secret)));
    let rc = recovery.map(|secret| (secret, register_start(secret)));
    client
        .send(&ClientFrame::Reregister {
            password: pw.as_ref().map(|(_, (_, request))| request.clone()),
            recovery: rc.as_ref().map(|(_, (_, request))| request.clone()),
        })
        .await;
    let ServerFrame::Registration { password: pw_response, recovery: rc_response } = client.recv().await else {
        panic!("expected registration");
    };
    let finish = |started: Option<(&str, _)>, response: Option<_>| {
        let (secret, (state, _)) = started?;
        Some(register_finish_with(&recovery_ksf(), state, secret, response.expect("a response for each request")).0)
    };
    (finish(pw, pw_response), finish(rc, rc_response))
}

/// Sends `resume` on a new connection; returns it and the answer.
pub async fn resume(server: &TestServer, token: &B64<32>, device_id: &B64<16>) -> (Client, ServerFrame) {
    let mut client = Client::open(server).await;
    client.send(&ClientFrame::Resume { token: token.clone(), device_id: device_id.clone() }).await;
    let answer = client.recv().await;
    (client, answer)
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
    pub async fn try_sign_up(
        server: &TestServer,
        base: &str,
        device: Device,
    ) -> Result<(Person, Client), Box<ServerFrame>> {
        let mut client = Client::open(server).await;
        let begun = begin_signup(&mut client, base, "Abin", device).await?;
        let ClientFrame::SignupFinish { password_copy, recovery_copy, .. } = &begun.finish else {
            unreachable!("begin_signup makes a signup_finish");
        };
        let (password_copy, recovery_copy) = (password_copy.0.clone(), recovery_copy.0.clone());
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
            exchange: begun.exchange,
            password_copy,
            recovery_copy,
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

    /// The public keys sent at signup.
    pub fn public_keys(&self) -> PublicKeys {
        PublicKeys {
            signing: B64(self.keys.verifying_key().to_bytes()),
            exchange: B64(self.exchange),
            signature: B64(self.keys.sign(&exchange_key_proof(&self.exchange)).to_bytes()),
        }
    }

    /// The account id.
    pub async fn id(&self, server: &TestServer) -> i64 {
        let (name, now) = (self.username.clone(), server.state.clock.now());
        server.state.db.call(move |c| accounts::by_username(c, &name, now).unwrap().expect("the account").id).await
    }

    /// Wrong passwords since the last successful sign-in.
    pub async fn wrong_count(&self, server: &TestServer) -> i64 {
        let (name, now) = (self.username.clone(), server.state.clock.now());
        server
            .state
            .db
            .call(move |c| accounts::by_username(c, &name, now).unwrap().expect("the account").wrong_count)
            .await
    }

    /// A new connection resumed with the signup session.
    pub async fn client_for(&self, server: &TestServer) -> Client {
        match resume(server, &self.token, &self.device.id).await {
            (client, ServerFrame::SignedIn { .. }) => client,
            (_, other) => panic!("expected signed_in, got {other:?}"),
        }
    }

    /// Signs in from `device` on a new connection: the connection and its token.
    pub async fn sign_in(&self, server: &TestServer, device: &Device) -> (Client, B64<32>) {
        let mut client = Client::open(server).await;
        match login(&mut client, &self.username, &self.password, device).await {
            Outcome::Server(ServerFrame::SignedIn { token, .. }) => (client, token),
            other => panic!("sign-in failed: {other:?}"),
        }
    }

    /// Five wrong passwords, the 15-minute lock passes, five more: locked for good.
    pub async fn lock_for_good(&self, server: &TestServer) {
        let mut client = Client::open(server).await;
        for round in 0..2 {
            for _ in 0..5 {
                let outcome = login(&mut client, &self.username, "wrong", &device("thief")).await;
                assert!(matches!(outcome, Outcome::ClientRefused), "got {outcome:?}");
            }
            if round == 0 {
                server.state.clock.advance(server.state.limits.lock_secs);
            }
        }
    }
}
