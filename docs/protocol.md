# The corvo-chat protocol

This is what a client (the CLI, and later the website and the phone app)
needs to talk to a corvo-chat server. It covers accounts (signing up,
invites, signing in, devices, recovery) and chats (people, messages, marks,
syncing every device, presence). The Rust types for every frame
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
- After any `signed_in` of an active account, send `sync` (section 11).
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

## 7. People

Each account has its own **row** about each other account it knows. The
other side never sees your row directly, only its effects:

| Your row about them | You may | They see your display name | They see your marks (`✓`, `✓✓`) | Online status |
|---|---|---|---|---|
| none (a stranger) | message them, which asks them | no | no | no |
| `asked` (you messaged first) | message them | yes | no | no |
| `request` (they messaged first) | `accept`, `reject`, `block` | no | no | no |
| `contact` | message, `remove`, `block` | yes | yes | yes, if they count you as a contact too |
| `blocked` | `unblock` | no | no | no |

- **`lookup {number}`** → `found {number, public_keys}`: the public keys
  only, never the display name. At most 30 lookups an hour. Your own
  number, a pending account or an unknown number gets `number_unknown`.
- **A first message** to a stranger makes your row `asked` and theirs
  `request`. At most 20 open requests per account, with 20 messages each;
  beyond that, messages are **withheld** (see Sending).
- **Two people asking each other** (you message someone whose row about you
  is `asked`) become contacts.
- **`accept {number}`:** both become contacts; they learn your name, and
  your marks so far appear at once.
- **`reject {number}`:** your row goes, and your side of the chat is
  cleared. They aren't told; their next message is a new request.
- **`block {number}`:** nothing from them reaches you, and they aren't told
  (they see you offline). **`unblock`** makes them a stranger again.
- **`remove {number, clear_history}`:** your row goes, and with
  `clear_history` your side of the chat is cleared. If they message again,
  it's a new request.
- **Activation by invite** makes the inviter and the new account contacts.
- These answer only with events (a `person` event to every device of
  yours); a refusal is an `error`.

## 8. Chats and keys

- One chat per pair of accounts. Its **chat id** is 16 random bytes the
  creating client makes, since messages use it before the server has seen
  the chat.
- The **chat key** (32 random bytes) is sealed separately to each member's
  X25519 key. The server stores both **sealed copies** as they come (at
  most 1 024 bytes each) and never opens them.
- The creator signs, with its Ed25519 key, the bytes of
  `corvo-chat chat v1` (ASCII) followed by: the chat id; the creator's
  number as 10 ASCII digits; the other number the same way; then each
  sealed copy (the creator's first) as a 2-byte big-endian length and its
  bytes. The server checks the signature at creation; clients check it too.
- Each message has a 16-byte **message id** from the sender (a ULID), and
  its associated data is `chat id ‖ sender number ‖ message id`.
- A message body is at most **64 KiB** of ciphertext; a larger one fails as
  `bad_frame`.

## 9. Sending

```
C: send      {chat_id, msg_id, body, new_chat: {number, my_key, their_key, signature} or null}
S: sent      {chat_id, msg_id, seq, sent_at}
   not_sent  {chat_id, msg_id, code, message}
```
- `new_chat` goes with the chat's first message only. If the two already
  have a chat (both started one at the same moment), the answer is
  `not_sent` with `chat_exists`, and the existing chat arrives as an event:
  encrypt the message again with its key and send it there.
- **Resending** after a reconnect (the same `msg_id` in the same chat)
  returns the first `sent` and stores nothing new. Messages marked `○` can
  always be resent.
- `seq` numbers a chat's messages from 1.
- **Withheld:** a message to someone who blocked you, or beyond the request
  limits, still gets `sent`, but never reaches them. It stays `·`.
- **Refusals** (`✗`): `chat_unknown`, `number_unknown`, `not_allowed`
  (`Accept the request first.`, `Unblock them first.`), `rate_limited`
  (60 messages a minute; 20 new people a day), `bad_frame` (a bad
  signature, a taken chat id).

## 10. Marks

```
C: received  {chat_id, through}
C: read      {chat_id, through}
```
- A device sends `received` once it has the messages through `through`, and
  `read` once they're read. Neither gets a reply; both only move forward,
  and `read` also counts as received.
- The other side's cursors come as `marks {received_through,
  read_through}`: the first device to receive makes your message `✓`, a
  read makes it `✓✓`.
- Marks reach you only while the other side has you as a contact, so a
  request stays `·`, even when it's been read. On accept, the marks so far
  arrive at once.
- A `read` also reaches your own other devices as a `read` event, so they
  clear the unread count.

## 11. Sync

Every change to an account goes into its **event log**, numbered from 1.
Each event carries the thing's current state, so applying an event twice
does no harm.

```
C: sync     {cursor}                  the last seq this device has
S: events   {events, cursor}          pages of the log after it
S: synced   {cursor}
S: presence {number, online: true}    one per contact online
S: events   {events, cursor}          live, from then on
```
- **Right after `signed_in`, send `sync`.** No events or presence arrive
  before it.
- A page holds at most 100 events, or stops once its message bodies reach
  512 KiB. `cursor` is the last seq the frame covers; events you may no
  longer see (a cleared message) are left out, but the cursor still moves
  past them. Keep the cursor only after handling a whole frame.
- **A new device** sends `sync {cursor: null}` and gets
  `snapshot {cursor, account, people, chats, pins}`, then one
  `history {chat_id, messages, complete}` per chat with its latest 50
  messages, then `synced`. A cursor the log never reached (a restored
  server) gets the same.
- **Older messages:** `history {chat_id, before, limit}` (at most 50) →
  `history {chat_id, messages, complete}`, oldest first. `complete` means
  there's nothing older to show.

Event kinds (`{"seq": 7, "event": {"kind": …}}`):

| `kind` | Fields | When |
|---|---|---|
| `message` | `message` | A message in one of your chats, yours included |
| `chat` | `chat` | A chat appeared for you |
| `person` | `person` | Your row about someone changed, or what you may see of them |
| `marks` | `chat_id, marks` | The other side's cursors moved |
| `read` | `chat_id, read_through` | You read further on another device |
| `cleared` | `chat_id, cleared_through` | Your side of a chat was cleared up to here |
| `profile` | `account` | Your display name changed |
| `devices` | `devices` | A device signed in or out, or was renamed |
| `pin` | `pin` | A pinned key changed |

A `chat` holds both sealed copies (`keys`, the creator's first), the
signature, `last_seq`, your own `read_through` and `cleared_through`, and the
other side's `marks` when they share them. A `person` holds the number, the
display name when they share it, their public keys, and your `relation`.

## 12. Presence, pins and names

- **Presence:** `presence {number, online}` whenever a mutual contact's
  first device connects or last one leaves. There's no "last seen".
- **Pins:** `set_pin {pin: {number, pin}}` stores your pinned keys for a
  number, encrypted with your data key (at most 1 024 bytes); `pin: null`
  removes them. Every device of yours gets a `pin` event, and a snapshot
  holds them all.
- **Display name:** `set_display_name {display_name}` → a `profile` event
  on your devices and a `person` event for everyone who sees your name.
- **Device name:** `rename_device {name}` renames this device → a `devices`
  event on all your devices.

## 13. Every frame

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
| `sync` | Active | `{"type":"sync","cursor":41}` |
| `history` | Active | `{"type":"history","chat_id":"<b64:16>","before":71,"limit":50}` |
| `lookup` | Active | `{"type":"lookup","number":"4829105572"}` |
| `send` | Active | `{"type":"send","chat_id":"<b64:16>","msg_id":"<b64:16>","body":"<b64>","new_chat":null}` |
| `received` | Active | `{"type":"received","chat_id":"<b64:16>","through":12}` |
| `read` | Active | `{"type":"read","chat_id":"<b64:16>","through":12}` |
| `accept` / `reject` / `block` / `unblock` | Active | `{"type":"accept","number":"4829105572"}` |
| `remove` | Active | `{"type":"remove","number":"4829105572","clear_history":false}` |
| `set_display_name` | Active | `{"type":"set_display_name","display_name":"Abin"}` |
| `rename_device` | Active | `{"type":"rename_device","name":"work laptop"}` |
| `set_pin` | Active | `{"type":"set_pin","pin":{"number":"4829105572","pin":"<b64>"}}` |

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
| `snapshot` | `{"type":"snapshot","snapshot":{"cursor":41,"account":{…},"people":[…],"chats":[…],"pins":[…]}}` |
| `events` | `{"type":"events","events":[{"seq":42,"event":{"kind":"read","chat_id":"<b64:16>","read_through":12}}],"cursor":42}` |
| `synced` | `{"type":"synced","cursor":42}` |
| `history` | `{"type":"history","chat_id":"<b64:16>","messages":[{"chat_id":"<b64:16>","seq":12,"msg_id":"<b64:16>","from":"4829105572","body":"<b64>","sent_at":1800000000}],"complete":false}` |
| `found` | `{"type":"found","number":"4829105572","public_keys":{…}}` |
| `sent` | `{"type":"sent","chat_id":"<b64:16>","msg_id":"<b64:16>","seq":12,"sent_at":1800000000}` |
| `not_sent` | `{"type":"not_sent","chat_id":"<b64:16>","msg_id":"<b64:16>","code":"rate_limited","message":"Too many messages. Slow down."}` |
| `presence` | `{"type":"presence","number":"4829105572","online":true}` |

### Error codes

The `message` is meant to be shown as it is.

| Code | When | Message |
|---|---|---|
| `bad_frame` | Not a frame, a frame out of order, bytes that don't parse, a body over 64 KiB, or a chat signature that doesn't check out | — |
| `update_required` | `hello` with an old protocol version; the connection closes | `run /update` |
| `not_signed_in` | A frame that needs a session, before signing in | `sign in first` |
| `already_signed_in` | A sign-up, sign-in or resume on a signed-in connection | `already signed in` |
| `not_active` | A pending account sending anything but `activate` or `sign_out` | `the account is waiting for its invite code` |
| `bad_username` | A base that breaks the rules | why |
| `bad_display_name` | A display name that breaks the rules (signup or `set_display_name`) | why |
| `bad_device` | A device name that breaks the rules (signing in or `rename_device`) | why |
| `bad_public_keys` | The signature doesn't check out | `those keys don't check out` |
| `try_again` | Someone took the username or number in the moment between the offer and the finish | `Please sign up again.` |
| `signup_limit` | Too many signups from this device or IP today | `Only one new account per device per day.` / `Too many new accounts from this network today.` |
| `signup_expired` | The pending day ran out; `signed_out {expired}` follows | `Your signup expired. Sign up again.` |
| `wrong_credentials` | A finish that doesn't prove the secret, or an unknown username | `Username or password is wrong` |
| `locked` | 5 wrong passwords; `until` says when it opens | `Locked for 15 more minutes` |
| `locked_forever` | 10 wrong passwords | `Locked. Sign in with your recovery code` |
| `sign_in_limit` | The 6th sign-in today | `Too many sign-ins today, try again tomorrow.` |
| `device_limit` | A new device beyond the limit | `Maximum devices reached (3). Sign out a device first.` |
| `rate_limited` | Too many unknown usernames from this IP, or recovery tries for this account, in the last hour; 30 lookups an hour; 60 messages a minute; 20 new people a day | `Too many tries. Wait an hour.`, … |
| `session_unknown` | `resume` with a token that's gone or a different device id | `This device was signed out.` |
| `not_pending` | Inviting a number that isn't pending | `That number isn't waiting for an invite.` |
| `code_wrong` | A wrong invite code | `That code is wrong.` |
| `code_dead` | No code, or 5 wrong tries used it up | `That code no longer works. Ask for a new invite.` |
| `too_many_invites` | A 6th open invite | `You have too many open invites.` |
| `not_allowed` | `reregister` or a secret change without a reauth in the last 5 minutes; a message to someone you have a request from or blocked; an accept, reject, unblock or remove that doesn't fit your row | `enter your password again first`, `Accept the request first.`, … |
| `internal` | A server fault; try again | `server error, try again` |
| `number_unknown` | A number that isn't an active account, or your own | `That number isn't on corvo-chat.` |
| `chat_unknown` | A chat that doesn't exist or isn't yours | `No such chat.` |
| `chat_exists` | A second chat for the same two people (in `not_sent`) | `You already have a chat with them.` |
