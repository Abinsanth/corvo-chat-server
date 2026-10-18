//! Chats, messages, people and the account event log, as frames carry them.
//! The server stores the sealed keys, bodies and pins without reading them.

use serde::{Deserialize, Serialize};

use crate::frames::{Account, DeviceInfo};
use crate::{B64, Blob, Body, Number, PublicKeys};

/// One message in a chat. `seq` is the server's per-chat number; `msg_id`
/// is the sender's own id, part of the associated data.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Message {
    pub chat_id: B64<16>,
    pub seq: i64,
    pub msg_id: B64<16>,
    pub from: Number,
    pub body: Body,
    pub sent_at: i64,
}

/// The chat key sealed to one member.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SealedKey {
    pub number: Number,
    pub key: Blob,
}

/// What the first message of a chat carries. `number` is the other person;
/// `signature` is the sender's over [`crate::chat_proof`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NewChat {
    pub number: Number,
    pub my_key: Blob,
    pub their_key: Blob,
    pub signature: B64<64>,
}

/// How far one side of a chat has got: what any of its devices received,
/// and what it read.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Marks {
    pub received_through: i64,
    pub read_through: i64,
}

/// A chat as one member sees it. `keys` holds both sealed copies, the
/// creator's first. `read_through` and `cleared_through` are the viewer's
/// own; `marks` are the other side's, `None` unless they count the viewer as
/// a contact.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ChatInfo {
    pub chat_id: B64<16>,
    pub with: Number,
    pub keys: Vec<SealedKey>,
    pub signature: B64<64>,
    pub last_seq: i64,
    pub read_through: i64,
    pub cleared_through: i64,
    pub marks: Option<Marks>,
}

/// The viewer's own standing with someone.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// A stranger.
    None,
    /// The viewer messaged them first, and they haven't accepted.
    Asked,
    /// They messaged the viewer first, and the viewer hasn't accepted.
    Request,
    Contact,
    Blocked,
}

/// Someone the viewer knows of. `relation` is the viewer's side; the display
/// name is there only when they share it with the viewer.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Person {
    pub number: Number,
    pub display_name: Option<String>,
    pub public_keys: PublicKeys,
    pub relation: Relation,
}

/// The viewer's pinned keys for a number, encrypted by the client. `None`
/// means removed.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Pin {
    pub number: Number,
    pub pin: Option<Blob>,
}

/// One entry of the account's event log.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Event {
    pub seq: i64,
    pub event: EventBody,
}

/// What changed. Each carries the thing's current state.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventBody {
    Message {
        message: Message,
    },
    Chat {
        chat: ChatInfo,
    },
    Person {
        person: Person,
    },
    /// The other side's marks moved.
    Marks {
        chat_id: B64<16>,
        marks: Marks,
    },
    /// The viewer read further on another device.
    Read {
        chat_id: B64<16>,
        read_through: i64,
    },
    /// The viewer's side of the history was deleted up to here.
    Cleared {
        chat_id: B64<16>,
        cleared_through: i64,
    },
    Profile {
        account: Account,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    Pin {
        pin: Pin,
    },
}

/// Everything a new device starts from; live events follow from `cursor`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub cursor: i64,
    pub account: Account,
    pub people: Vec<Person>,
    pub chats: Vec<ChatInfo>,
    pub pins: Vec<Pin>,
}
