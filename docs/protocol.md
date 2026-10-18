# The corvo-chat protocol

This is what a client (the CLI, and later the website and the phone app)
needs to talk to a corvo-chat server. It covers accounts: signing up,
invites, signing in, devices, and recovery. The Rust types for every frame
are in the `corvo-chat-proto` crate, which builds for WebAssembly too.

## 1. Transport

- Open a WebSocket to `wss://chat.corvo.live/ws` (or your own server's
  domain). TLS ends in the server, with a Let's Encrypt certificate.
- Every frame is one **JSON text message** with a `"type"` field in
  snake_case. Unknown fields are ignored, so new fields can be added without
  breaking older clients. A binary message, or text that isn't a frame, gets
  `bad_frame` and the connection stays open.
- **Bytes** are padded standard base64 (`A–Z a–z 0–9 + /`, with `=`). Fixed
  sizes are checked: a value of the wrong length fails the whole frame. In
  the examples, `"<b64:32>"` stands for the base64 of 32 bytes.
- **Times** are Unix seconds.
- **Optional fields** are `null` when absent, except `until` in `error`,
  which is left out.
- A message may be at most **128 KiB**.
- **Timers:**
  - A connection that isn't signed in is closed **5 minutes** after it
    opened.
  - Once signed in, the server pings every **30 seconds** and closes the
    connection after **90 seconds** with nothing received (pongs count).
    WebSocket libraries answer pings on their own.
  - The server gives up on a client that doesn't read what it's sent within
    10 seconds.

## 2. Versions

The first frame must be `hello`. The current protocol version is **1**.

```json
{"type":"hello","protocol_version":1,"app_version":"0.1.0"}
```

- A supported version gets `ready`:
  `{"type":"ready","server_version":"0.1.0"}`
- An older version gets an `update_required` error (message `run /update`),
  and the connection closes.
- Anything else first gets `bad_frame`, and the connection closes.

## 3. Accounts

**Username:** a **base** the person picks, a dash, and a 4-character
**ending** the server adds, such as `abin-7k3q`.
- Base: 3–20 characters from `a–z 0–9 _ .`, starting with a letter. The
  server trims it and lowercases it.
- Ending: 4 characters from `0123456789abcdefghjkmnpqrstvwxyz` (Crockford
  base32, lowercase, no `i l o u`). The server draws endings until the
  username is free, so signup never says a name is taken.
- Sign-in trims and lowercases the username too.

**Number:** 10 digits, shown as `482 910 5572`. The first 9 are random (the
first one 1–9), and the 10th is a **Damm** check digit. Accept spaces and
dashes, and check the digit on the client before sending a number
(`that number has a typo`). On the wire a number is the 10-digit string,
such as `"4829105572"`.

Damm table (row = interim digit, column = next digit):
```
0 3 1 7 5 9 8 6 4 2
7 0 9 2 1 5 4 8 6 3
4 2 0 6 8 7 1 3 5 9
1 7 5 0 9 8 3 4 2 6
6 1 2 3 0 4 5 9 7 8
3 6 7 4 2 0 9 5 8 1
5 8 6 9 7 2 0 1 3 4
8 9 4 5 3 6 2 0 1 7
9 4 3 8 6 1 7 2 0 5
2 5 8 1 4 3 6 7 9 0
```
Start at interim 0 and look up each digit in turn. The check digit is the
interim after the 9th digit, and a whole number is valid when all 10 digits
end at 0.

**Display name:** 1–32 characters, no control characters, trimmed.

**Device:** each install makes a random 16-byte **device id** on its first
start and keeps it. It goes with a kind (`cli`, `web`, `phone`) and a name of
1–64 characters (the CLI uses the hostname):
```json
{"id":"<b64:16>","kind":"cli","name":"abins-laptop"}
```
An account has at most **3 devices** signed in (the server can be set up
with another number). The same device id signing in again replaces its old
session instead of taking a new place.

**Pending and active:** a new account is **pending** for a day. It can sign
in and resume, but only `activate` or `sign_out` until someone active
invites it. `account` objects
look like this, with `pending_until` `null` once active:
```json
{"username":"abin-7k3q","number":"4829105572","display_name":"Abin","pending_until":1800086400}
```

## 4. Keys

**OPAQUE** (RFC 9807, the `opaque-ke` 4 crate) with:
- OPRF: ristretto255;
- key exchange: TripleDH over ristretto255 with SHA-512;
- key stretching: Argon2id, version 0x13. For the **password**:
  m = 65536 KiB (64 MiB), t = 3, p = 1. For the **recovery code**: m = 8 KiB,
  t = 1, p = 1. The recovery code has 120 random bits, so it needs no slow
  step.
- No custom identifiers and no context.

The server registers each account twice, under the credential ids
`password:<username>` and `recovery:<username>`. The client doesn't send
these; the server picks them from the username.

Message sizes: registration request 32 bytes, response 64, upload 192;
credential request 96, response 320, finalization 64.

**Wrap key:** HKDF-SHA256 over the OPAQUE export key, no salt, info
`corvo-chat wrap v1`, 32 bytes. The password's wrap key and the recovery
code's wrap key are different.

**Copies:** the **password copy** and the **recovery copy** are the account
secrets (the Ed25519 and X25519 key pairs and the data key), encrypted with
XChaCha20-Poly1305 under the matching wrap key. The client decides their
layout. The server stores them as they come and hands them back; each is at
most 1024 bytes.

**Public keys:**
```json
{"signing":"<b64:32>","exchange":"<b64:32>","signature":"<b64:64>"}
```
`signing` is the Ed25519 public key and `exchange` the X25519 public key.
`signature` is Ed25519 by `signing` over the bytes of
`corvo-chat exchange key v1` (ASCII, no terminator) followed by the 32
bytes of `exchange`. The server checks it strictly (`verify_strict`) at
signup.

## 5. Flows

`C:` is the client and `S:` the server. Every flow starts after `hello` and
`ready`.

### Sign up

```
C: signup_start   {base, display_name, device, password: reg request, recovery: reg request}
S: signup_offer   {username, number, password: reg response, recovery: reg response}
C: signup_finish  {password: reg upload, recovery: reg upload, password_copy, recovery_copy, public_keys}
S: signed_in      {token, account (pending), copy: null}
```
- The client makes the account secrets and the recovery code before
  `signup_finish`, and shows the username next to the recovery code.
- Keep the **token** (32 bytes) with the device id: it's how this device
  resumes. The server keeps only its SHA-256.
- If `signup_finish` gets `try_again`, start over with `signup_start`.

### Invite and activate

The pending person tells their number to someone active, who sends:
```
C: invite   {number}
S: invited  {invite: {number, display_name, code, expires_at}}
```
The inviter checks the display name and passes on the 6-digit `code`. Then
the pending person:
```
C: activate   {code}
S: activated  {account (active)}
```
- `list_invites` gives `invites {invites: [...]}`: the inviter's open
  invites, oldest first.
- A user has at most 5 open invites. Inviting the same number again makes a
  new code, and the old one stops working.
- 5 wrong codes kill the code (`code_dead`); the inviter has to invite again.
- The code lives as long as the pending account. The server admin can also
  activate or invite with `corvo-chat-server activate <number>` and
  `invite <number>`.

### Sign in

```
C: login_start   {username, request: cred request, device}
S: login_offer   {response: cred response}
C: login_finish  {finalization}
S: signed_in     {token, account, copy: password copy}
```
The client unlocks the password copy with the wrap key.

### Resume

A device that's already signed in reconnects without the password:
```
C: resume     {token, device_id}
S: signed_in  {token, account, copy: null}
```
- The CLI keeps the password copy itself and unlocks it with the password
  offline, so it doesn't need the copy here.
- `session_unknown` means the device was signed out: delete the local cache
  and keys, and go back to signing in or up.
- A resume isn't a sign-in: no lock or limit applies to it.
- Resuming from a second connection closes the first one, without a frame.

### Devices

```
C: list_devices     {}
S: devices          {devices: [{id, kind, name, first_seen, last_used, this_device}, ...]}
C: sign_out_device  {id}
S: devices          {devices: [...]}
```
- The list is oldest first, and `this_device` marks the asking device.
- The signed-out device, if connected, gets `signed_out {reason:
  "by_another_device"}` and is closed. Signing out your own id this way is
  `bad_frame`: use `sign_out`. An unknown id leaves the list unchanged.
- A device unused for 30 days is signed out. A connected device counts as
  used.

### Prove the password again, then change a secret

```
C: reauth_start   {request}
S: login_offer    {response}
C: reauth_finish  {finalization}
S: reauthed       {}
```
For the next **5 minutes** the device may send:
```
C: reregister         {password: reg request or null, recovery: reg request or null}
S: registration       {password: reg response or null, recovery: reg response or null}
C: change_password    {password: reg upload, password_copy}
S: password_changed   {}
C: new_recovery_code  {recovery: reg upload, recovery_copy}
S: recovery_code_changed {}
```
- A password change signs out every other device (`signed_out {reason:
  "password_changed"}`).
- A new recovery code leaves the other devices signed in, and the old code
  stops working.
- A reauth counts toward the password lock like a sign-in. A failure leaves
  the device signed in.

### Recovery

For a forgotten password, a lock, or a lost or stolen device:
```
C: recovery_start     {username, request, device}
S: login_offer        {response}
C: recovery_finish    {finalization}
S: recovery_offer     {recovery_copy}
C: reregister         {password: reg request, recovery: reg request}
S: registration       {password: reg response, recovery: reg response}
C: recovery_complete  {password: reg upload, recovery: reg upload, password_copy, recovery_copy}
S: signed_in          {token, account, copy: null}
```
- The client asks for a new password and makes a **new recovery code**,
  since the old one is used up.
- `recovery_complete` replaces both secrets, clears any lock, and signs out
  **every** device (`signed_out {reason: "recovered"}`); then this device
  signs in.
- No lock, daily limit or device limit applies. Nothing changes until
  `recovery_complete`, so an abandoned recovery leaves the account as it was.

### Sign out

```
C: sign_out    {}
S: signed_out  {reason: "this_device"}
```
The connection then closes.

## 6. Wrong passwords

- **Only the client sees a wrong password:** its OPAQUE `finish` fails. It
  must then **not** send `login_finish` (or `recovery_finish`,
  `reauth_finish`) and shows `Username or password is wrong`. It can send a
  new start on the same connection.
- So the server counts **every** `login_start` and `reauth_start` against a
  real account as a wrong password, and clears the count when a login
  finishes. A start that's never finished stays counted.
- **5 wrong → locked for 15 minutes** (`locked`, with `until`).
- **5 more wrong → locked for good** (`locked_forever`) until the recovery
  code is used. There's no other way to unlock.
- The count has no time window: it's the wrong passwords since the last
  successful sign-in.
- **Locks never touch signed-in devices:** resume and everything else keep
  working, except a reauth, which needs the password.
- **Unknown usernames** get a fake `login_offer` that looks like a real one;
  the finish always fails with `wrong_credentials`. They count against the
  IP: after **20 in an hour**, every `login_start` and `recovery_start` from
  that IP gets `rate_limited` for the rest of the hour, real usernames too.
- **Limits checked after the password is proven:** 5 successful sign-ins
  per account per day (`sign_in_limit`), and the device limit
  (`device_limit`).
- **Recovery:** 5 tries per account per hour (`rate_limited`).
- **Signup:** 1 per device id and 3 per IP per day (`signup_limit`).

## 7. Every frame

### Client frames

| Frame | When | Example |
|---|---|---|
| `hello` | First | `{"type":"hello","protocol_version":1,"app_version":"0.1.0"}` |
| `signup_start` | Not signed in | `{"type":"signup_start","base":"abin","display_name":"Abin","device":{…},"password":"<b64:32>","recovery":"<b64:32>"}` |
| `signup_finish` | After `signup_offer` | `{"type":"signup_finish","password":"<b64:192>","recovery":"<b64:192>","password_copy":"<b64>","recovery_copy":"<b64>","public_keys":{…}}` |
| `login_start` | Not signed in | `{"type":"login_start","username":"abin-7k3q","request":"<b64:96>","device":{…}}` |
| `login_finish` | After its `login_offer` | `{"type":"login_finish","finalization":"<b64:64>"}` |
| `recovery_start` | Not signed in | `{"type":"recovery_start","username":"abin-7k3q","request":"<b64:96>","device":{…}}` |
| `recovery_finish` | After its `login_offer` | `{"type":"recovery_finish","finalization":"<b64:64>"}` |
| `resume` | Not signed in | `{"type":"resume","token":"<b64:32>","device_id":"<b64:16>"}` |
| `reauth_start` | Signed in, active | `{"type":"reauth_start","request":"<b64:96>"}` |
| `reauth_finish` | After its `login_offer` | `{"type":"reauth_finish","finalization":"<b64:64>"}` |
| `reregister` | Reauthed, or recovering | `{"type":"reregister","password":"<b64:32>","recovery":null}` |
| `change_password` | Reauthed | `{"type":"change_password","password":"<b64:192>","password_copy":"<b64>"}` |
| `new_recovery_code` | Reauthed | `{"type":"new_recovery_code","recovery":"<b64:192>","recovery_copy":"<b64>"}` |
| `recovery_complete` | Recovering | `{"type":"recovery_complete","password":"<b64:192>","recovery":"<b64:192>","password_copy":"<b64>","recovery_copy":"<b64>"}` |
| `activate` | Signed in, pending | `{"type":"activate","code":"482913"}` |
| `invite` | Active | `{"type":"invite","number":"4829105572"}` |
| `list_invites` | Active | `{"type":"list_invites"}` |
| `list_devices` | Active | `{"type":"list_devices"}` |
| `sign_out_device` | Active | `{"type":"sign_out_device","id":"<b64:16>"}` |
| `sign_out` | Signed in | `{"type":"sign_out"}` |

### Server frames

| Frame | Example |
|---|---|
| `ready` | `{"type":"ready","server_version":"0.1.0"}` |
| `signup_offer` | `{"type":"signup_offer","username":"abin-7k3q","number":"4829105572","password":"<b64:64>","recovery":"<b64:64>"}` |
| `login_offer` | `{"type":"login_offer","response":"<b64:320>"}` |
| `signed_in` | `{"type":"signed_in","token":"<b64:32>","account":{…},"copy":"<b64>"}` (`copy` is `null` after signup, resume and recovery) |
| `recovery_offer` | `{"type":"recovery_offer","recovery_copy":"<b64>"}` |
| `registration` | `{"type":"registration","password":"<b64:64>","recovery":null}` |
| `reauthed` | `{"type":"reauthed"}` |
| `password_changed` | `{"type":"password_changed"}` |
| `recovery_code_changed` | `{"type":"recovery_code_changed"}` |
| `activated` | `{"type":"activated","account":{…}}` |
| `invited` | `{"type":"invited","invite":{"number":"4829105572","display_name":"Rahul","code":"482913","expires_at":1800086400}}` |
| `invites` | `{"type":"invites","invites":[{…}]}` |
| `devices` | `{"type":"devices","devices":[{"id":"<b64:16>","kind":"cli","name":"abins-laptop","first_seen":1800000000,"last_used":1800000000,"this_device":true}]}` |
| `signed_out` | `{"type":"signed_out","reason":"by_another_device"}`. Reasons: `this_device`, `by_another_device`, `password_changed`, `recovered`, `expired`. The connection closes after it |
| `error` | `{"type":"error","code":"locked","message":"Locked for 15 more minutes","until":1800000900}` |

### Error codes

The `message` is meant to be shown as it is.

| Code | When | Message |
|---|---|---|
| `bad_frame` | Not a frame, a frame out of order, or bytes that don't parse | — |
| `update_required` | `hello` with an old protocol version; the connection closes | `run /update` |
| `not_signed_in` | A frame that needs a session, before signing in | `sign in first` |
| `already_signed_in` | A sign-up, sign-in or resume on a signed-in connection | `already signed in` |
| `not_active` | A pending account sending anything but `activate` or `sign_out` | `the account is waiting for its invite code` |
| `bad_username` | A base that breaks the rules | why |
| `bad_display_name` | A display name that breaks the rules | why |
| `bad_device` | A device name that breaks the rules | why |
| `bad_public_keys` | The signature doesn't check out | `those keys don't check out` |
| `try_again` | Someone took the username or number in the moment between the offer and the finish | `Please sign up again.` |
| `signup_limit` | Too many signups from this device or IP today | `Only one new account per device per day.` / `Too many new accounts from this network today.` |
| `signup_expired` | The pending day ran out; `signed_out {expired}` follows | `Your signup expired. Sign up again.` |
| `wrong_credentials` | A finish that doesn't prove the secret, or an unknown username | `Username or password is wrong` |
| `locked` | 5 wrong passwords; `until` says when it opens | `Locked for 15 more minutes` |
| `locked_forever` | 10 wrong passwords | `Locked. Sign in with your recovery code` |
| `sign_in_limit` | The 6th sign-in today | `Too many sign-ins today, try again tomorrow.` |
| `device_limit` | A new device beyond the limit | `Maximum devices reached (3). Sign out a device first.` |
| `rate_limited` | Too many unknown usernames from this IP, or recovery tries for this account, in the last hour | `Too many tries. Wait an hour.` |
| `session_unknown` | `resume` with a token that's gone or a different device id | `This device was signed out.` |
| `not_pending` | Inviting a number that isn't pending | `That number isn't waiting for an invite.` |
| `code_wrong` | A wrong invite code | `That code is wrong.` |
| `code_dead` | No code, or 5 wrong tries used it up | `That code no longer works. Ask for a new invite.` |
| `too_many_invites` | A 6th open invite | `You have too many open invites.` |
| `not_allowed` | `reregister` or a secret change without a reauth in the last 5 minutes | `enter your password again first` |
| `internal` | A server fault; try again | `server error, try again` |
