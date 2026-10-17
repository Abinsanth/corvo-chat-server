use serde::{Deserialize, Serialize};

use crate::keys::PublicKeys;
use crate::opaque::{CredFinal, CredRequest, CredResponse, RegRequest, RegResponse, RegUpload};
use crate::{B64, Blob, Number};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Cli,
    Web,
    Phone,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Device {
    pub id: B64<16>,
    pub kind: DeviceKind,
    pub name: String,
}

/// `pending_until: None` means the account is active.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Account {
    pub username: String,
    pub number: Number,
    pub display_name: String,
    pub pending_until: Option<i64>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct InviteInfo {
    pub number: Number,
    pub display_name: String,
    pub code: String,
    pub expires_at: i64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: B64<16>,
    pub kind: DeviceKind,
    pub name: String,
    pub first_seen: i64,
    pub last_used: i64,
    pub this_device: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignedOutReason {
    ThisDevice,
    ByAnotherDevice,
    PasswordChanged,
    Recovered,
    Expired,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadFrame,
    UpdateRequired,
    NotSignedIn,
    AlreadySignedIn,
    NotActive,
    BadUsername,
    BadDisplayName,
    BadDevice,
    BadPublicKeys,
    TryAgain,
    SignupLimit,
    SignupExpired,
    WrongCredentials,
    Locked,
    LockedForever,
    SignInLimit,
    DeviceLimit,
    RateLimited,
    SessionUnknown,
    NotPending,
    CodeWrong,
    CodeDead,
    TooManyInvites,
    NotAllowed,
    Internal,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// First frame.
    Hello { protocol_version: u32, app_version: String },
    /// Anonymous.
    SignupStart { base: String, display_name: String, device: Device, password: RegRequest, recovery: RegRequest },
    /// After `signup_offer`.
    SignupFinish {
        password: RegUpload,
        recovery: RegUpload,
        password_copy: Blob,
        recovery_copy: Blob,
        public_keys: PublicKeys,
    },
    /// Anonymous.
    LoginStart { username: String, request: CredRequest, device: Device },
    /// After `login_offer`.
    LoginFinish { finalization: CredFinal },
    /// Anonymous.
    RecoveryStart { username: String, request: CredRequest, device: Device },
    /// After its `login_offer`.
    RecoveryFinish { finalization: CredFinal },
    /// Anonymous.
    Resume { token: B64<32>, device_id: B64<16> },
    /// Signed in, active.
    ReauthStart { request: CredRequest },
    /// After its `login_offer`.
    ReauthFinish { finalization: CredFinal },
    /// Recovering, or reauthed.
    Reregister { password: Option<RegRequest>, recovery: Option<RegRequest> },
    /// Recovering.
    RecoveryComplete { password: RegUpload, recovery: RegUpload, password_copy: Blob, recovery_copy: Blob },
    /// Reauthed.
    ChangePassword { password: RegUpload, password_copy: Blob },
    /// Reauthed.
    NewRecoveryCode { recovery: RegUpload, recovery_copy: Blob },
    /// Signed in, pending.
    Activate { code: String },
    /// Active.
    Invite { number: Number },
    /// Active.
    ListInvites,
    /// Active.
    ListDevices,
    /// Active.
    SignOutDevice { id: B64<16> },
    /// Signed in.
    SignOut,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    Ready {
        server_version: String,
    },
    SignupOffer {
        username: String,
        number: Number,
        password: RegResponse,
        recovery: RegResponse,
    },
    LoginOffer {
        response: CredResponse,
    },
    /// `copy` is the password copy after a login; `None` after signup and resume.
    SignedIn {
        token: B64<32>,
        account: Account,
        copy: Option<Blob>,
    },
    RecoveryOffer {
        recovery_copy: Blob,
    },
    Registration {
        password: Option<RegResponse>,
        recovery: Option<RegResponse>,
    },
    Reauthed,
    PasswordChanged,
    RecoveryCodeChanged,
    Activated {
        account: Account,
    },
    Invited {
        invite: InviteInfo,
    },
    Invites {
        invites: Vec<InviteInfo>,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    SignedOut {
        reason: SignedOutReason,
    },
    /// `until` is only set with `locked`.
    Error {
        code: ErrorCode,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        until: Option<i64>,
    },
}
