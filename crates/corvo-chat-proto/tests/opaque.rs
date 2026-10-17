use corvo_chat_proto::B64;
use corvo_chat_proto::opaque::{Suite, password_id, password_ksf, recovery_id, recovery_ksf, wrap_key};
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration, ClientRegistrationFinishParameters,
    CredentialFinalization, CredentialRequest, CredentialResponse, Identifiers, RegistrationRequest,
    RegistrationResponse, RegistrationUpload, ServerLogin, ServerLoginParameters, ServerRegistration, ServerSetup,
};
use rand_core::OsRng;

/// Passes a message through the fixed-size wire type, as a frame would.
fn b64<const N: usize>(bytes: impl AsRef<[u8]>) -> B64<N> {
    B64(bytes.as_ref().try_into().unwrap())
}

const USER: &str = "abin-7k3q";

/// Registers `password` and returns the server's stored record and the wrap key.
fn register(setup: &ServerSetup<Suite>, password: &[u8]) -> (Vec<u8>, [u8; 32]) {
    let id = recovery_id(USER);
    let ksf = recovery_ksf();
    let start = ClientRegistration::<Suite>::start(&mut OsRng, password).unwrap();
    let request = b64::<32>(start.message.serialize());
    let request = RegistrationRequest::<Suite>::deserialize(&request.0).unwrap();
    let response = ServerRegistration::<Suite>::start(setup, request, &id).unwrap();
    let response = b64::<64>(response.message.serialize());
    let response = RegistrationResponse::<Suite>::deserialize(&response.0).unwrap();
    let params = ClientRegistrationFinishParameters::new(Identifiers::default(), Some(&ksf));
    let finish = start.state.finish(&mut OsRng, password, response, params).unwrap();
    let upload = b64::<192>(finish.message.serialize());
    let upload = RegistrationUpload::<Suite>::deserialize(&upload.0).unwrap();
    let record = ServerRegistration::<Suite>::finish(upload).serialize().to_vec();
    (record, wrap_key(&finish.export_key))
}

/// Logs in; the client's `finish` error (a wrong password) comes back as `Err`.
fn login(
    setup: &ServerSetup<Suite>,
    record: &[u8],
    password: &[u8],
) -> Result<[u8; 32], opaque_ke::errors::ProtocolError> {
    let id = recovery_id(USER);
    let ksf = recovery_ksf();
    let start = ClientLogin::<Suite>::start(&mut OsRng, password).unwrap();
    let request = b64::<96>(start.message.serialize());
    let request = CredentialRequest::<Suite>::deserialize(&request.0).unwrap();
    let record = ServerRegistration::<Suite>::deserialize(record).unwrap();
    let server =
        ServerLogin::start(&mut OsRng, setup, Some(record), request, &id, ServerLoginParameters::default()).unwrap();
    let response = b64::<320>(server.message.serialize());
    let response = CredentialResponse::<Suite>::deserialize(&response.0).unwrap();
    let params = ClientLoginFinishParameters::new(None, Identifiers::default(), Some(&ksf));
    let finish = start.state.finish(&mut OsRng, password, response, params)?;
    let fin = b64::<64>(finish.message.serialize());
    let fin = CredentialFinalization::<Suite>::deserialize(&fin.0).unwrap();
    let done = server.state.finish(fin, ServerLoginParameters::default()).unwrap();
    assert_eq!(done.session_key, finish.session_key);
    Ok(wrap_key(&finish.export_key))
}

#[test]
fn register_then_login_gives_the_same_wrap_key() {
    let setup = ServerSetup::<Suite>::new(&mut OsRng);
    let (record, registered) = register(&setup, b"correct horse");
    assert_eq!(record.len(), 192);
    assert_eq!(login(&setup, &record, b"correct horse").unwrap(), registered);
}

#[test]
fn wrong_password_fails_on_the_client() {
    let setup = ServerSetup::<Suite>::new(&mut OsRng);
    let (record, _) = register(&setup, b"correct horse");
    assert!(login(&setup, &record, b"wrong horse").is_err());
}

#[test]
fn password_ksf_has_the_spec_values() {
    let params = password_ksf().params().clone();
    assert_eq!((params.m_cost(), params.t_cost(), params.p_cost()), (65_536, 3, 1));
}

#[test]
fn ids_keep_password_and_recovery_apart() {
    assert_eq!(password_id("abin-7k3q"), b"password:abin-7k3q");
    assert_ne!(password_id("abin-7k3q"), recovery_id("abin-7k3q"));
}
