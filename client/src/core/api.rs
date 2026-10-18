//! Every HTTP call to the control server and to MediaMTX's WHIP/WHEP endpoints.

use super::types::*;
use super::upload::{Progress, stoppable};
use parking_lot::RwLock;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct ApiError {
    pub status: u16,
    /// The server's `error` field, or one of ours.
    pub code: ErrorCode,
    pub message: String,
    pub body: Value,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    fn unreachable(base: &str) -> ApiError {
        ApiError {
            status: 0,
            code: ErrorCode::Unreachable,
            message: trf!(
                "Can't reach {}. Check the address and that the server is running.",
                "Não foi possível acessar {}. Confira o endereço e se o servidor está rodando.",
                base
            ),
            body: Value::Null,
        }
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

/// Whether signing in to this address would send the password readable to anyone on the way: plain
/// HTTP to a host on the internet. A LAN, this computer and a Tailscale network (WireGuard
/// underneath) are not that.
pub fn sends_in_the_clear(address: &str) -> bool {
    use std::net::IpAddr;
    let Ok(url) = url::Url::parse(&normalize_base(address)) else { return false };
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) => !nearby(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => !nearby(IpAddr::V6(ip)),
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            let local = [".local", ".lan", ".home", ".internal", ".home.arpa", ".ts.net"];
            !(name == "localhost" || !name.contains('.') || local.iter().any(|s| name.ends_with(s)))
        }
        None => false,
    }
}

/// Loopback, private, link-local, or Tailscale's 100.64.0.0/10 and fd7a:115c:a1e0::/48.
fn nearby(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || (a == 100 && (64..128).contains(&b))
        }
        std::net::IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(|v4| nearby(v4.into()))
        }
    }
}

/// Adds `http://` when there is no scheme and drops trailing slashes, as the old client did.
pub fn normalize_base(url: &str) -> String {
    let url = url.trim();
    let with_scheme = if url.contains("://") { url.to_string() } else { format!("http://{url}") };
    with_scheme.trim_end_matches('/').to_string()
}

#[derive(Default)]
struct Credentials {
    base: String,
    password: String,
    token: String,
}

/// The TLS every connection to a server is made with: rustls on ring, and certificates checked
/// the way the system checks them (its own store, its revocation, the roots an admin installed),
/// so a server with a certificate from a CA put on this computer by hand works as it did with the
/// system's TLS before. Nothing here accepts a certificate the system would not.
///
/// The first call also makes ring the process's provider, for anything that builds a client
/// without a configuration of its own (the updater): rustls refuses to pick one by itself.
fn tls_base() -> &'static rustls::ClientConfig {
    static BASE: OnceLock<rustls::ClientConfig> = OnceLock::new();
    BASE.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .expect("ring does TLS 1.2 and 1.3");
        match rustls_platform_verifier::Verifier::new(provider) {
            Ok(verifier) => builder.dangerous().with_custom_certificate_verifier(Arc::new(verifier)).with_no_client_auth(),
            // Without the system's checks nothing could be trusted: HTTPS fails, plain HTTP works.
            Err(e) => {
                log::error!("the system's certificate checks are not available, so no HTTPS server can be reached: {e}");
                builder.with_root_certificates(rustls::RootCertStore::empty()).with_no_client_auth()
            }
        }
    })
}

/// `tls_base`, offering these application protocols.
pub fn tls(alpn: &[&[u8]]) -> rustls::ClientConfig {
    let mut config = tls_base().clone();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    config
}

/// Makes ring the process's TLS provider. Done by the first `Api` anyway; anything that may make a
/// client of its own before one exists calls this first.
pub fn install_tls() {
    tls_base();
}

/// Whether a redirect from `from` may be followed with the server password and the sign-in on it:
/// only to the same host, on the same scheme and port, or the same host moving up to HTTPS. The
/// password goes in a header of our own, which an HTTP client carries to wherever it is sent; a
/// reverse proxy sending it to another host would hand that host the password.
fn may_follow(from: &url::Url, to: &url::Url) -> bool {
    let same_host = from.host_str().is_some_and(|h| to.host_str().is_some_and(|t| t.eq_ignore_ascii_case(h)));
    let same_place = from.scheme() == to.scheme() && from.port_or_known_default() == to.port_or_known_default();
    let upgrade = from.scheme() == "http" && to.scheme() == "https";
    same_host && (same_place || upgrade)
}

/// The most a file may be when nothing better is known: the largest the server lets an owner allow.
const MOST_FILE_BYTES: u64 = 2 << 30;
/// What a file sealed on its way gains: the version, the nonce and the tag.
const SEALING: u64 = 1 << 10;
/// The most a picture to draw may be (an avatar, an emoji, a picture in the chat): the server
/// takes avatars and emoji of 256 KB, and a picture bigger than this is not one to decode.
pub const PICTURE_BYTES: u64 = 64 << 20;

#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    creds: Arc<RwLock<Credentials>>,
    /// The largest file this server could hand back, from its settings; 0 until they are known.
    file_limit: Arc<AtomicU64>,
}

impl Api {
    pub fn new() -> Api {
        // No read timeout here: reqwest's runs from the request's start until the response, an
        // upload included, so it would cut off any upload slower than it. Each request sets its
        // own limit, and downloads give up on a stall instead (see `file`).
        let http = reqwest::Client::builder()
            .user_agent(concat!("Ciranda/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .tls_backend_preconfigured(tls(&[b"h2", b"http/1.1"]))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let follow = attempt.previous().len() < 10 && attempt.previous().first().is_none_or(|from| may_follow(from, attempt.url()));
                if follow { attempt.follow() } else { attempt.stop() }
            }))
            .build()
            .expect("http client");
        Api { http, creds: Default::default(), file_limit: Default::default() }
    }

    /// What the server's settings say about files: no file it hands back is bigger than the most
    /// its owner may allow. Not the limit set now, which an owner may have lowered since a bigger
    /// file was sent.
    pub fn note_file_limits(&self, info: &ServerInfo) {
        let most = info.max_file_limit.into_iter().chain(info.max_file_bytes).max().unwrap_or(0);
        self.file_limit.store(most, Ordering::Relaxed);
    }

    /// The most a file downloaded from this server may be, sealed or not.
    fn file_cap(&self) -> u64 {
        let most = match self.file_limit.load(Ordering::Relaxed) {
            0 => MOST_FILE_BYTES,
            n => n,
        };
        most + SEALING
    }

    pub fn set_server(&self, base: &str, password: &str) {
        let mut c = self.creds.write();
        c.base = normalize_base(base);
        c.password = password.to_string();
    }

    pub fn set_token(&self, token: &str) {
        self.creds.write().token = token.to_string();
    }

    pub fn token(&self) -> String {
        self.creds.read().token.clone()
    }

    pub fn base(&self) -> String {
        self.creds.read().base.clone()
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let c = self.creds.read();
        let mut rb = rb;
        if !c.password.is_empty() {
            rb = rb.header("X-Harmony-Password", &c.password);
        }
        if !c.token.is_empty() {
            rb = rb.bearer_auth(&c.token);
        }
        rb
    }

    async fn send<T: DeserializeOwned>(&self, rb: reqwest::RequestBuilder, timeout: Duration) -> Result<T> {
        let base = self.base();
        let res = self.authed(rb).timeout(timeout).send().await.map_err(|_| ApiError::unreachable(&base))?;
        let status = res.status().as_u16();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            let code = body.get("error").and_then(Value::as_str).map(ErrorCode::parse).unwrap_or(ErrorCode::Unknown);
            let message = message_for(code, status, &body);
            return Err(ApiError { status, code, message, body });
        }
        serde_json::from_value(body.clone()).map_err(|e| ApiError {
            status,
            code: ErrorCode::BadReply,
            message: trf!(
                "The server sent something this client does not understand ({}).",
                "O servidor enviou algo que este cliente não entende ({}).",
                e
            ),
            body,
        })
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{path}", self.base());
        self.send(self.http.get(url), Duration::from_secs(10)).await
    }

    async fn post<T: DeserializeOwned>(&self, path: &str, body: impl Serialize) -> Result<T> {
        let url = format!("{}{path}", self.base());
        self.send(self.http.post(url).json(&body), Duration::from_secs(10)).await
    }

    // Health, accounts and the server itself.

    pub async fn health(&self) -> Result<Health> {
        self.get("/api/health").await
    }

    pub async fn inbox(&self) -> Result<Inbox> {
        self.get("/api/inbox").await
    }

    pub async fn register(&self, nickname: &str, password: &str, owner_key: Option<&str>) -> Result<AuthReply> {
        self.post("/api/accounts/register", json!({ "nickname": nickname, "password": password, "ownerKey": owner_key })).await
    }

    pub async fn login(&self, nickname: &str, password: &str, owner_key: Option<&str>) -> Result<AuthReply> {
        self.post("/api/accounts/login", json!({ "nickname": nickname, "password": password, "ownerKey": owner_key })).await
    }

    pub async fn logout(&self) -> Result<Value> {
        self.post("/api/accounts/logout", json!({})).await
    }

    pub async fn me(&self) -> Result<User> {
        #[derive(serde::Deserialize)]
        struct R {
            user: User,
        }
        Ok(self.get::<R>("/api/accounts/me").await?.user)
    }

    pub async fn set_avatar(&self, hash: Option<&str>) -> Result<User> {
        Ok(self.post::<UserReply>("/api/accounts/avatar", json!({ "hash": hash })).await?.user)
    }

    pub async fn set_display_name(&self, name: &str) -> Result<User> {
        Ok(self.post::<UserReply>("/api/accounts/display-name", json!({ "displayName": name })).await?.user)
    }

    /// Every session of the account ends, this one included; the reply is this one's new token.
    pub async fn change_password(&self, current: &str, password: &str) -> Result<String> {
        #[derive(serde::Deserialize)]
        struct R {
            token: String,
        }
        Ok(self.post::<R>("/api/accounts/password", json!({ "current": current, "password": password })).await?.token)
    }

    pub async fn users(&self) -> Result<Vec<User>> {
        #[derive(serde::Deserialize)]
        struct R {
            #[serde(deserialize_with = "lenient")]
            users: Vec<User>,
        }
        Ok(self.get::<R>("/api/accounts").await?.users)
    }

    pub async fn set_role(&self, user: UserId, role: Role) -> Result<User> {
        Ok(self.post::<UserReply>(&format!("/api/accounts/{user}/role"), json!({ "role": role })).await?.user)
    }

    pub async fn delete_account(&self, user: UserId) -> Result<Value> {
        self.post(&format!("/api/accounts/{user}/delete"), json!({})).await
    }

    pub async fn server(&self) -> Result<ServerInfo> {
        Ok(self.get::<ServerReply>("/api/server").await?.server)
    }

    /// `password: None` leaves it alone; `Some("")` removes it.
    pub async fn update_server(&self, name: &str, password: Option<&str>) -> Result<ServerInfo> {
        let mut body = json!({ "name": name });
        if let Some(p) = password {
            body["password"] = json!(p);
        }
        Ok(self.post::<ServerReply>("/api/server", body).await?.server)
    }

    /// `None` takes the picture off. Sent under both names: a server of the Electron line calls it
    /// `logo`, and this line's takes `iconHash` first.
    pub async fn set_server_icon(&self, hash: Option<&str>) -> Result<ServerInfo> {
        Ok(self.post::<ServerReply>("/api/server", json!({ "iconHash": hash, "logo": hash })).await?.server)
    }

    /// The largest file anybody may send; `None` puts the server's default back.
    pub async fn set_max_file_bytes(&self, bytes: Option<u64>) -> Result<ServerInfo> {
        Ok(self.post::<ServerReply>("/api/server", json!({ "maxFileBytes": bytes })).await?.server)
    }

    pub async fn storage(&self) -> Result<Storage> {
        self.get("/api/server/storage").await
    }

    // The flat namespace.

    pub async fn session(&self, username: &str, camera: bool) -> Result<SessionReply> {
        let mut body = json!({ "username": username });
        if camera {
            body["kind"] = json!("camera");
        }
        self.post("/api/session", body).await
    }

    pub async fn release_session(&self, username: &str, token: &str) -> Result<Value> {
        self.post("/api/session/release", json!({ "username": username, "token": token })).await
    }

    pub async fn streams(&self) -> Result<(Vec<LiveStream>, Vec<IceServer>)> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct R {
            streams: Vec<LiveStream>,
            #[serde(default)]
            ice_servers: Vec<IceServer>,
        }
        let r: R = self.get("/api/streams").await?;
        Ok((r.streams, r.ice_servers))
    }

    // Channels.

    pub async fn channels(&self) -> Result<Snapshot> {
        self.get("/api/channels").await
    }

    pub async fn create_channel(&self, kind: ChannelKind, name: &str, password: Option<&str>) -> Result<Channel> {
        Ok(self.post::<ChannelReply>("/api/channels", json!({ "kind": kind, "name": name, "password": password })).await?.channel)
    }

    pub async fn update_channel(&self, id: ChannelId, name: &str, password: Option<&str>) -> Result<Channel> {
        let mut body = json!({ "name": name });
        if let Some(p) = password {
            body["password"] = json!(p);
        }
        Ok(self.post::<ChannelReply>(&format!("/api/channels/{id}"), body).await?.channel)
    }

    pub async fn set_channel_mic_locked(&self, id: ChannelId, locked: bool) -> Result<Channel> {
        Ok(self.post::<ChannelReply>(&format!("/api/channels/{id}"), json!({ "micLocked": locked })).await?.channel)
    }

    pub async fn delete_channel(&self, id: ChannelId) -> Result<Value> {
        self.post(&format!("/api/channels/{id}/delete"), json!({})).await
    }

    pub async fn create_group(&self, name: &str) -> Result<Value> {
        self.post("/api/channels/groups", json!({ "name": name })).await
    }

    pub async fn rename_group(&self, id: i64, name: &str) -> Result<Value> {
        self.post(&format!("/api/channels/groups/{id}"), json!({ "name": name })).await
    }

    pub async fn delete_group(&self, id: i64) -> Result<Value> {
        self.post(&format!("/api/channels/groups/{id}/delete"), json!({})).await
    }

    /// Positions follow list order. `channels` is every channel, in drawing order.
    pub async fn arrange(&self, groups: &[i64], channels: &[(ChannelId, Option<i64>)]) -> Result<Value> {
        let channels: Vec<Value> = channels.iter().map(|(id, g)| json!({ "id": id, "groupId": g })).collect();
        self.post("/api/channels/arrange", json!({ "groups": groups, "channels": channels })).await
    }

    // Chat.

    pub async fn messages(&self, channel: ChannelId, before: Option<MessageId>) -> Result<History> {
        let q = before.map(|b| format!("?before={b}")).unwrap_or_default();
        self.get(&format!("/api/channels/{channel}/messages{q}")).await
    }

    pub async fn send_message(
        &self,
        channel: ChannelId,
        body: &str,
        attachment: Option<(&str, &str)>,
        reply_to: Option<MessageId>,
    ) -> Result<Message> {
        let mut b = json!({ "body": body });
        if let Some((hash, name)) = attachment {
            b["attachmentHash"] = json!(hash);
            b["attachmentName"] = json!(name);
        }
        if let Some(id) = reply_to {
            b["replyTo"] = json!(id);
        }
        Ok(self.post::<MessageReply>(&format!("/api/channels/{channel}/messages"), b).await?.message)
    }

    pub async fn pin(&self, id: MessageId, pinned: bool) -> Result<Message> {
        Ok(self.post::<MessageReply>(&format!("/api/messages/{id}/pin"), json!({ "pinned": pinned })).await?.message)
    }

    pub async fn edit(&self, id: MessageId, body: &str) -> Result<Message> {
        Ok(self.post::<MessageReply>(&format!("/api/messages/{id}/edit"), json!({ "body": body })).await?.message)
    }

    /// Every text a message has had, oldest first, the current one last.
    pub async fn versions(&self, id: MessageId) -> Result<Vec<Version>> {
        Ok(self.get::<VersionsReply<Version>>(&format!("/api/messages/{id}/edits")).await?.versions)
    }

    pub async fn replace_attachment(&self, id: MessageId, hash: Option<&str>, name: Option<&str>) -> Result<Message> {
        Ok(self.post::<MessageReply>(&format!("/api/messages/{id}/attachment"), json!({ "hash": hash, "name": name })).await?.message)
    }

    pub async fn delete_message(&self, id: MessageId) -> Result<Value> {
        self.post(&format!("/api/messages/{id}/delete"), json!({})).await
    }

    pub async fn react(&self, id: MessageId, emoji: &str, on: bool) -> Result<Value> {
        self.post(&format!("/api/messages/{id}/react"), json!({ "emoji": emoji, "on": on })).await
    }

    pub async fn search(&self, channel: ChannelId, q: &str) -> Result<SearchReply> {
        let q = url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>();
        self.get(&format!("/api/channels/{channel}/search?q={q}")).await
    }

    // Private conversations: keys.

    pub async fn keys(&self) -> Result<KeysReply> {
        self.get("/api/keys").await
    }

    pub async fn lookup_keys(&self, ids: &[KeyId]) -> Result<Vec<PublicKeyInfo>> {
        #[derive(serde::Deserialize)]
        struct R {
            #[serde(deserialize_with = "lenient")]
            keys: Vec<PublicKeyInfo>,
        }
        let ids: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        Ok(self.get::<R>(&format!("/api/keys/lookup?ids={}", ids.join(","))).await?.keys)
    }

    pub async fn publish_key(&self, public_key: &str, wrapped: &str) -> Result<OwnKey> {
        Ok(self.post::<KeyReply>("/api/keys", json!({ "publicKey": public_key, "wrapped": wrapped })).await?.key)
    }

    pub async fn rewrap_key(&self, key_id: KeyId, wrapped: &str) -> Result<OwnKey> {
        Ok(self.post::<KeyReply>("/api/keys/wrap", json!({ "keyId": key_id, "wrapped": wrapped })).await?.key)
    }

    // Private conversations.

    pub async fn dms(&self) -> Result<DmList> {
        self.get("/api/dms").await
    }

    pub async fn open_dm(&self, user: UserId) -> Result<Conversation> {
        Ok(self.post::<ConversationReply>("/api/dms", json!({ "userId": user })).await?.conversation)
    }

    pub async fn dm_messages(&self, id: ConversationId, before: Option<MessageId>) -> Result<DmHistory> {
        let q = before.map(|b| format!("?before={b}")).unwrap_or_default();
        self.get(&format!("/api/dms/{id}/messages{q}")).await
    }

    pub async fn send_dm(&self, id: ConversationId, sealed: &Sealed, attachment: Option<&str>) -> Result<SealedMessage> {
        let body = json!({
            "sealed": sealed.text, "senderKey": sealed.sender_key, "recipientKey": sealed.recipient_key,
            "attachmentHash": attachment,
        });
        Ok(self.post::<SealedReply>(&format!("/api/dms/{id}/messages"), body).await?.message)
    }

    pub async fn edit_dm(&self, message: MessageId, sealed: &Sealed) -> Result<SealedMessage> {
        let body = json!({ "sealed": sealed.text, "senderKey": sealed.sender_key, "recipientKey": sealed.recipient_key });
        Ok(self.post::<SealedReply>(&format!("/api/dm-messages/{message}/edit"), body).await?.message)
    }

    /// A private message's earlier texts, sealed, oldest first; the current one is the message.
    pub async fn dm_versions(&self, message: MessageId) -> Result<Vec<SealedVersion>> {
        Ok(self.get::<VersionsReply<SealedVersion>>(&format!("/api/dm-messages/{message}/edits")).await?.versions)
    }

    pub async fn delete_dm(&self, message: MessageId) -> Result<Value> {
        self.post(&format!("/api/dm-messages/{message}/delete"), json!({})).await
    }

    /// `None` clears your reactions to the message.
    pub async fn react_dm(&self, message: MessageId, sealed: Option<&Sealed>) -> Result<Value> {
        let body = match sealed {
            Some(s) => json!({ "sealed": s.text, "senderKey": s.sender_key, "recipientKey": s.recipient_key }),
            None => json!({ "sealed": null }),
        };
        self.post(&format!("/api/dm-messages/{message}/react"), body).await
    }

    pub async fn read_dm(&self, id: ConversationId, up_to: MessageId) -> Result<Value> {
        self.post(&format!("/api/dms/{id}/read"), json!({ "upTo": up_to })).await
    }

    /// Deletes everything in the conversation for you; the other person keeps it.
    pub async fn clear_dm(&self, id: ConversationId) -> Result<Value> {
        self.post(&format!("/api/dms/{id}/clear"), json!({})).await
    }

    pub async fn set_blocked(&self, user: UserId, blocked: bool) -> Result<Vec<UserId>> {
        #[derive(serde::Deserialize)]
        struct R {
            blocked: Vec<UserId>,
        }
        Ok(self.post::<R>("/api/dms/blocks", json!({ "userId": user, "blocked": blocked })).await?.blocked)
    }

    /// Uploads an attachment already sealed on this computer.
    pub async fn upload_sealed(&self, id: ConversationId, bytes: Vec<u8>) -> Result<Upload> {
        let url = format!("{}/api/dms/{id}/uploads", self.base());
        let limit = transfer_timeout(bytes.len());
        self.send(self.http.post(url).header("Content-Type", "application/octet-stream").body(bytes), limit).await
    }

    /// A sealed attachment, still sealed: it is opened by whoever holds its key.
    pub async fn download_sealed(&self, id: ConversationId, hash: &str) -> Result<(Vec<u8>, String)> {
        self.file(&format!("/api/dms/{id}/uploads/{hash}"), self.file_cap()).await
    }

    /// As `download_sealed`, for a picture to draw (`PICTURE_BYTES`).
    pub async fn download_sealed_picture(&self, id: ConversationId, hash: &str) -> Result<(Vec<u8>, String)> {
        self.file(&format!("/api/dms/{id}/uploads/{hash}"), self.file_cap().min(PICTURE_BYTES + SEALING)).await
    }

    // Files.

    pub async fn upload(&self, bytes: Vec<u8>, content_type: &str) -> Result<Upload> {
        let url = format!("{}/api/uploads", self.base());
        let limit = transfer_timeout(bytes.len());
        self.send(self.http.post(url).header("Content-Type", content_type).body(bytes), limit).await
    }

    // Files in pieces: see core/upload.rs, which drives these.

    pub async fn open_upload(&self, size: u64, content_type: &str) -> Result<UploadSession> {
        self.post("/api/uploads/sessions", json!({ "size": size, "contentType": content_type })).await
    }

    /// One piece at `offset`; the reply is how much of the file the server now has.
    pub async fn upload_chunk(&self, id: &str, offset: u64, bytes: Vec<u8>) -> Result<u64> {
        #[derive(serde::Deserialize)]
        struct R {
            received: u64,
        }
        let url = format!("{}/api/uploads/sessions/{id}/chunk?offset={offset}", self.base());
        let limit = transfer_timeout(bytes.len());
        let rb = self.http.post(url).header("Content-Type", "application/octet-stream").body(bytes);
        Ok(self.send::<R>(rb, limit).await?.received)
    }

    /// The last step: the file is put in place, which on a slow disk takes a moment for a big one.
    pub async fn finish_upload(&self, id: &str) -> Result<Upload> {
        let url = format!("{}/api/uploads/sessions/{id}/finish", self.base());
        self.send(self.http.post(url).json(&json!({})), Duration::from_secs(60)).await
    }

    /// Best effort: a session nobody finishes is forgotten by the server anyway.
    pub async fn cancel_upload(&self, id: &str) {
        let _ = self.post::<Value>(&format!("/api/uploads/sessions/{id}/cancel"), json!({})).await;
    }

    pub async fn download(&self, hash: &str) -> Result<(Vec<u8>, String)> {
        self.file(&format!("/api/uploads/{hash}"), self.file_cap()).await
    }

    /// A stored file that is to be drawn: an avatar, an emoji, a picture in the chat. Refused
    /// past `PICTURE_BYTES`, which no picture worth decoding comes near.
    pub async fn download_picture(&self, hash: &str) -> Result<(Vec<u8>, String)> {
        self.file(&format!("/api/uploads/{hash}"), self.file_cap().min(PICTURE_BYTES)).await
    }

    /// A stored file saved straight to `path` as it arrives, so a 2 GB one never sits in memory.
    ///
    /// Not through the cache: a file being saved is not one being looked at, and a big one would
    /// push everything else out of it. Written beside `path` first and renamed once it has all
    /// come and matches its hash, so a download that breaks or is cancelled never leaves half a
    /// file under the name asked for. `size` is what the message said, for a reply without a length.
    pub async fn download_to(&self, hash: &str, path: &std::path::Path, size: Option<u64>, progress: &Progress) -> Result<u64> {
        let part = {
            let mut name = path.file_name().unwrap_or_default().to_os_string();
            name.push(".part");
            path.with_file_name(name)
        };
        let got = stoppable(progress, self.download_part(hash, &part, size, progress)).await;
        match got {
            Ok(n) => std::fs::rename(&part, path).map(|_| n).map_err(|e| broken_download(e.to_string())),
            Err(e) => {
                // A cancel can land while a write still holds the file open, and Windows won't
                // delete an open file: a few tries, a moment apart.
                let _ = tokio::task::spawn_blocking(move || {
                    for _ in 0..20 {
                        if std::fs::remove_file(&part).is_ok() || !part.exists() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                })
                .await;
                Err(e)
            }
        }
    }

    async fn download_part(&self, hash: &str, part: &std::path::Path, size: Option<u64>, progress: &Progress) -> Result<u64> {
        use sha2::Digest;
        let base = self.base();
        let url = format!("{base}/api/uploads/{hash}");
        let unreachable = || ApiError::unreachable(&base);
        let mut res = tokio::time::timeout(STALL, self.authed(self.http.get(url)).send())
            .await
            .map_err(|_| unreachable())?
            .map_err(|_| unreachable())?;
        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(ApiError {
                status,
                code: ErrorCode::NoSuchFile,
                message: message_for(ErrorCode::NoSuchFile, status, &Value::Null),
                body: Value::Null,
            });
        }
        let cap = self.file_cap();
        if res.content_length().is_some_and(|n| n > cap) {
            return Err(too_big());
        }
        progress.total(res.content_length().or(size).unwrap_or(0));
        let mut file = Some(std::fs::File::create(part).map_err(|e| broken_download(e.to_string()))?);
        let mut digest = sha2::Sha256::new();
        let mut buf = Vec::with_capacity(WRITE_BYTES);
        let mut received = 0u64;
        loop {
            let chunk = tokio::time::timeout(STALL, res.chunk()).await.map_err(|_| unreachable())?.map_err(|_| unreachable())?;
            let done = chunk.is_none();
            if let Some(chunk) = chunk {
                if received + chunk.len() as u64 > cap {
                    return Err(too_big());
                }
                digest.update(&chunk);
                received += chunk.len() as u64;
                buf.extend_from_slice(&chunk);
            }
            // To the disk a megabyte at a time, on a blocking thread: a disk that is slow or on
            // the network must not hold up the runtime every other request runs on.
            if buf.len() >= WRITE_BYTES || (done && !buf.is_empty()) {
                let (mut f, bytes) =
                    (file.take().expect("file between writes"), std::mem::replace(&mut buf, Vec::with_capacity(WRITE_BYTES)));
                file = Some(
                    tokio::task::spawn_blocking(move || {
                        use std::io::Write;
                        f.write_all(&bytes).map(|_| f)
                    })
                    .await
                    .map_err(|e| broken_download(e.to_string()))?
                    .map_err(|e| broken_download(e.to_string()))?,
                );
                progress.sent(received);
            }
            if done {
                break;
            }
        }
        // Content-addressed: the name is the hash of the bytes, so a file that arrived damaged
        // (or cut short by a proxy that says nothing) is caught here rather than opened later.
        if hex::encode(digest.finalize()) != hash {
            return Err(broken_download(tr!("it arrived damaged", "chegou corrompido").into()));
        }
        drop(file);
        Ok(received)
    }

    /// The server's picture, which needs no account: the sign-in screen shows it. A server of the
    /// Electron line keeps its one picture at `/api/server/logo` instead.
    pub async fn server_icon(&self, hash: &str) -> Result<(Vec<u8>, String)> {
        match self.file(&format!("/api/server/icon/{hash}"), PICTURE_BYTES).await {
            Err(e) if e.status == 404 => self.file("/api/server/logo", PICTURE_BYTES).await,
            got => got,
        }
    }

    /// A file whole, in memory. Given up on as soon as more than `cap` bytes came: the hash is only
    /// checked at the end, so a server sending without end would otherwise fill the memory first.
    async fn file(&self, path: &str, cap: u64) -> Result<(Vec<u8>, String)> {
        let base = self.base();
        let url = format!("{base}{path}");
        // No limit on the time it takes: a 25 MB file from a Pi at home can take minutes. What ends
        // one is the server not answering, or the file not moving, for `STALL`.
        let unreachable = || ApiError::unreachable(&base);
        let mut res = tokio::time::timeout(STALL, self.authed(self.http.get(url)).send())
            .await
            .map_err(|_| unreachable())?
            .map_err(|_| unreachable())?;
        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(ApiError {
                status,
                code: ErrorCode::NoSuchFile,
                message: message_for(ErrorCode::NoSuchFile, status, &Value::Null),
                body: Value::Null,
            });
        }
        let ct = res.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
        if res.content_length().is_some_and(|n| n > cap) {
            return Err(too_big());
        }
        let mut bytes = Vec::with_capacity(res.content_length().unwrap_or(0).min(64 << 20) as usize);
        while let Some(chunk) = tokio::time::timeout(STALL, res.chunk()).await.map_err(|_| unreachable())?.map_err(|_| unreachable())? {
            if (bytes.len() + chunk.len()) as u64 > cap {
                return Err(too_big());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes, ct))
    }

    // Emoji and soundpad.

    pub async fn emojis(&self) -> Result<Vec<CustomEmoji>> {
        #[derive(serde::Deserialize)]
        struct R {
            #[serde(deserialize_with = "lenient")]
            emojis: Vec<CustomEmoji>,
        }
        Ok(self.get::<R>("/api/emojis").await?.emojis)
    }

    pub async fn add_emoji(&self, name: &str, hash: &str) -> Result<Value> {
        self.post("/api/emojis", json!({ "name": name, "hash": hash })).await
    }

    pub async fn delete_emoji(&self, id: i64) -> Result<Value> {
        self.post(&format!("/api/emojis/{id}/delete"), json!({})).await
    }

    pub async fn soundpad(&self) -> Result<Vec<Clip>> {
        #[derive(serde::Deserialize)]
        struct R {
            #[serde(deserialize_with = "lenient")]
            clips: Vec<Clip>,
        }
        Ok(self.get::<R>("/api/soundpad").await?.clips)
    }

    pub async fn add_clip(&self, name: &str, emoji: Option<&str>, hash: &str) -> Result<Value> {
        self.post("/api/soundpad", json!({ "name": name, "emoji": emoji, "hash": hash })).await
    }

    pub async fn rename_clip(&self, id: i64, name: &str, emoji: Option<&str>) -> Result<Value> {
        self.post(&format!("/api/soundpad/{id}/rename"), json!({ "name": name, "emoji": emoji })).await
    }

    pub async fn delete_clip(&self, id: i64) -> Result<Value> {
        self.post(&format!("/api/soundpad/{id}/delete"), json!({})).await
    }

    pub async fn reorder_clips(&self, ids: &[i64]) -> Result<Value> {
        self.post("/api/soundpad/reorder", json!({ "ids": ids })).await
    }

    // WHIP and WHEP.

    /// POSTs a complete offer and returns the answer and the session's resource URL.
    pub async fn sdp_exchange(&self, url: &str, offer: &str) -> Result<(String, Option<String>)> {
        let res = self
            .http
            .post(url)
            .header("Content-Type", "application/sdp")
            .body(offer.to_string())
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| ApiError {
                status: 0,
                code: ErrorCode::MediaUnreachable,
                message: tr!("Can't reach the media server.", "Não foi possível acessar o servidor de mídia.").into(),
                body: Value::Null,
            })?;
        let status = res.status().as_u16();
        let location = res.headers().get("location").and_then(|v| v.to_str().ok()).map(str::to_string);
        let text = res.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            let code = match status {
                401 | 403 => ErrorCode::Unauthorized,
                404 => ErrorCode::NotLive,
                _ => ErrorCode::MediaError,
            };
            return Err(ApiError {
                status,
                code,
                message: trf!("The media server said {}.", "O servidor de mídia respondeu {}.", status),
                body: Value::String(text),
            });
        }
        let resource = location.and_then(|l| url::Url::parse(url).ok()?.join(&l).ok()).map(|u| u.to_string());
        Ok((text, resource))
    }

    /// Ends a WHIP or WHEP session. Best effort.
    pub async fn delete_resource(&self, url: &str) {
        let _ = self.http.delete(url).timeout(Duration::from_secs(5)).send().await;
    }
}

impl Default for Api {
    fn default() -> Self {
        Api::new()
    }
}

#[derive(serde::Deserialize)]
struct UserReply {
    user: User,
}

#[derive(serde::Deserialize)]
struct ServerReply {
    server: ServerInfo,
}

#[derive(serde::Deserialize)]
struct ChannelReply {
    channel: Channel,
}

#[derive(serde::Deserialize)]
struct MessageReply {
    message: Message,
}

#[derive(serde::Deserialize)]
struct KeyReply {
    key: OwnKey,
}

#[derive(serde::Deserialize)]
struct ConversationReply {
    conversation: Conversation,
}

#[derive(serde::Deserialize)]
struct SealedReply {
    message: SealedMessage,
}

/// Something sealed for a private conversation, and the two keys it was sealed to.
#[derive(Clone, Debug)]
pub struct Sealed {
    pub text: String,
    pub sender_key: KeyId,
    pub recipient_key: KeyId,
}

/// How long a download may go without a byte before it's taken for stalled.
const STALL: Duration = Duration::from_secs(30);

/// How much of a download saved to disk is held before it is written.
const WRITE_BYTES: usize = 1 << 20;

/// A download bigger than anything the server could have been sent: not kept.
fn too_big() -> ApiError {
    ApiError {
        status: 0,
        code: ErrorCode::FileTooLarge,
        message: tr!(
            "The server sent more than that file can be, so it was not kept.",
            "O servidor mandou mais do que esse arquivo pode ter, então ele não foi guardado."
        )
        .into(),
        body: Value::Null,
    }
}

fn broken_download(why: String) -> ApiError {
    ApiError {
        status: 0,
        code: ErrorCode::Unknown,
        message: trf!("The download broke off: {}", "O download foi interrompido: {}", why),
        body: Value::Null,
    }
}

/// How long an upload of `bytes` may take in all: a minute, or longer at 64 KB/s, so a big file
/// over a slow uplink isn't cut off while it's still going.
fn transfer_timeout(bytes: usize) -> Duration {
    Duration::from_secs(60).max(Duration::from_secs((bytes / (64 * 1024)) as u64))
}

/// The server's message for a failure: ours, in the reader's language, for every code we know;
/// the server's own (in English) for one we don't; the status when there is neither.
fn message_for(code: ErrorCode, status: u16, body: &Value) -> String {
    friendly(code, body)
        .or_else(|| body.get("message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| trf!("The server answered {}.", "O servidor respondeu {}.", status))
}

/// What to tell people about a code the server (HTTP or socket) refuses with.
pub fn friendly(code: ErrorCode, body: &Value) -> Option<String> {
    use ErrorCode as E;
    let text = match code {
        E::PasswordRequired => tr!("This server needs a password.", "Este servidor pede uma senha."),
        E::BadPassword => tr!("Wrong server password.", "Senha do servidor incorreta."),
        E::BadCredentials => tr!("Wrong username or password.", "Nome de usuário ou senha incorretos."),
        E::WeakPassword => tr!("Use at least 6 characters.", "Use pelo menos 6 caracteres."),
        E::LoginRequired => tr!("Sign in first.", "Entre na sua conta primeiro."),
        E::LockedOut => {
            let secs = body.get("retryAfterSec").and_then(Value::as_u64).unwrap_or(60);
            return Some(if secs < 90 {
                trf!("Too many wrong passwords. Try again in {} seconds.", "Senha errada vezes demais. Tente de novo em {} segundos.", secs)
            } else {
                trf!(
                    "Too many wrong passwords. Try again in {} minutes.",
                    "Senha errada vezes demais. Tente de novo em {} minutos.",
                    secs.div_ceil(60)
                )
            });
        }
        E::Forbidden => tr!("You are not allowed to do that.", "Você não tem permissão para fazer isso."),
        E::OwnerOnly => tr!("Only the owner can do that.", "Só o dono pode fazer isso."),
        E::LastOwner => tr!("The server needs at least one owner.", "O servidor precisa de pelo menos um dono."),
        E::CannotRemoveOwner => tr!("The owner's account can't be deleted.", "A conta do dono não pode ser apagada."),
        E::NicknameTaken => tr!("That username is taken. Try another one.", "Esse nome de usuário já existe. Tente outro."),
        E::InvalidNickname => tr!(
            "Use 2 to 20 letters, digits, - or _, starting with a letter or digit.",
            "Use de 2 a 20 letras, números, - ou _, começando com letra ou número."
        ),
        E::TooLong => tr!("That name is too long. 32 characters at most.", "Esse nome é longo demais. No máximo 32 caracteres."),
        E::BadCharacters => tr!("That name has characters that can't be shown.", "Esse nome tem caracteres que não dá para mostrar."),
        E::InvalidName => tr!("Give it a name of up to 32 characters.", "Dê um nome de até 32 caracteres."),
        E::NameTaken => tr!("Something else already has that name.", "Já existe outro item com esse nome."),
        E::NoSuchUser => tr!("That account is gone.", "Essa conta não existe mais."),
        E::NoSuchMember => tr!("They already left the channel.", "Essa pessoa já saiu do canal."),
        E::NotInChannel => tr!("You are not in that voice channel any more.", "Você não está mais nesse canal de voz."),
        E::NoSuchChannel => tr!("That channel is gone.", "Esse canal não existe mais."),
        E::NoSuchGroup => tr!("That group is gone.", "Esse grupo não existe mais."),
        E::NoSuchMessage => tr!("That message is gone.", "Essa mensagem não existe mais."),
        E::NoSuchEmoji => tr!("That emoji is gone.", "Esse emoji não existe mais."),
        E::NoSuchClip => tr!("That sound is gone.", "Esse som não existe mais."),
        E::NoSuchFile | E::NoSuchUpload | E::BadHash => {
            tr!("That file is gone from the server.", "Esse arquivo não existe mais no servidor.")
        }
        E::ChannelFull => tr!("That voice channel is full.", "Esse canal de voz está cheio."),
        E::ServerFull => tr!("The server is out of space for files.", "O servidor ficou sem espaço para arquivos."),
        E::DiskFull => tr!(
            "The server's disk is nearly full, so it isn't taking files.",
            "O disco do servidor está quase cheio, então ele não está aceitando arquivos."
        ),
        E::FileTooLarge => tr!("That file is too big for this server.", "Esse arquivo é grande demais para este servidor."),
        E::TooManyUploads => tr!("Finish or cancel your other uploads first.", "Termine ou cancele seus outros envios primeiro."),
        E::BadOffset | E::Incomplete | E::ChunkTooLarge | E::Aborted => {
            tr!("The upload was interrupted. Send it again.", "O envio foi interrompido. Envie de novo.")
        }
        E::BadLimit => tr!("The largest file has to be between 1 MB and 2 GB.", "O maior arquivo tem que ficar entre 1 MB e 2 GB."),
        E::Cancelled => tr!("Upload cancelled.", "Envio cancelado."),
        E::AvatarTooLarge | E::PictureTooLarge | E::EmojiTooLarge => {
            tr!("That picture is too big. 256 KB at most.", "Essa imagem é grande demais. No máximo 256 KB.")
        }
        E::ClipTooLarge => tr!("Sounds can be up to 2 MB.", "Os sons podem ter até 2 MB."),
        E::TooManyEmojis => {
            tr!("This server has all the custom emoji it can hold.", "Este servidor já tem o máximo de emojis personalizados.")
        }
        E::NotAnImage => tr!("That file is not a picture.", "Esse arquivo não é uma imagem."),
        E::NotAudio => tr!("That file is not audio.", "Esse arquivo não é de áudio."),
        E::TypeNotAllowed => tr!("The server does not take that kind of file.", "O servidor não aceita esse tipo de arquivo."),
        E::EmptyFile => tr!("That file is empty.", "Esse arquivo está vazio."),
        E::EmptyMessage => tr!("The message would be empty. Write something first.", "A mensagem ficaria vazia. Escreva algo antes."),
        E::MediaServerDown => {
            tr!(
                "The media server is not answering. Try again in a moment.",
                "O servidor de mídia não está respondendo. Tente de novo daqui a pouco."
            )
        }
        E::BadOrder => tr!("The list changed meanwhile. Try again.", "A lista mudou nesse meio-tempo. Tente de novo."),
        E::ServerError => tr!("Something went wrong on the server. Try again.", "Algo deu errado no servidor. Tente de novo."),
        E::UnknownType => tr!("This server is too old for that. Update it.", "Este servidor é antigo demais para isso. Atualize-o."),
        E::Offline => {
            tr!("Not connected to the server. Try again once it's back.", "Sem conexão com o servidor. Tente de novo quando ela voltar.")
        }
        E::Timeout => tr!("The server didn't answer. Try again.", "O servidor não respondeu. Tente de novo."),
        E::NoKey => tr!("Set up private messages first.", "Ative as mensagens privadas primeiro."),
        E::PeerHasNoKey => tr!(
            "They can't receive private messages yet: they need to update Ciranda and open it once.",
            "Essa pessoa ainda não pode receber mensagens privadas: precisa atualizar Ciranda e abrir o app uma vez."
        ),
        E::StaleKey => tr!("A security key changed. Try again.", "Uma chave de segurança mudou. Tente de novo."),
        E::Blocked => tr!("You can't send messages in this conversation.", "Não é possível enviar mensagens nesta conversa."),
        E::SlowDown => tr!("Too much at once. Wait a moment.", "Muita coisa de uma vez. Espere um pouco."),
        E::NoSuchConversation => tr!("That conversation is gone.", "Essa conversa não existe mais."),
        E::NoSuchCall | E::NotAnswered => tr!("That call has ended.", "Essa chamada já terminou."),
        E::PeerOffline => tr!("They're offline. They'll see you called.", "A pessoa está offline. Ela vai ver que você ligou."),
        E::NotYourself => tr!("That's you.", "Essa pessoa é você."),
        _ => return None,
    };
    Some(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server on this computer that answers every request with `body`, until the test ends.
    fn serving(body: Vec<u8>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut head = Vec::new();
                let mut byte = [0; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                    head.push(byte[0]);
                }
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(reply.as_bytes()).and_then(|_| stream.write_all(&body));
            }
        });
        base
    }

    #[test]
    fn a_download_goes_to_disk_whole_or_not_at_all() {
        use sha2::Digest;
        let body: Vec<u8> = (0..3 * WRITE_BYTES + 17).map(|i| (i % 251) as u8).collect();
        let hash = hex::encode(sha2::Sha256::digest(&body));
        let api = Api::new();
        api.set_server(&serving(body.clone()), "");
        let dir = std::env::temp_dir().join(format!("ciranda-download-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (good, bad) = (dir.join("good.bin"), dir.join("bad.bin"));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();

        let progress = Progress::default();
        assert_eq!(rt.block_on(api.download_to(&hash, &good, None, &progress)).unwrap(), body.len() as u64);
        assert_eq!(std::fs::read(&good).unwrap(), body);
        assert_eq!(progress.fraction(), 1.);

        // Not the bytes its name promises: nothing is left under the name, nor beside it.
        let wrong = "0".repeat(64);
        assert!(rt.block_on(api.download_to(&wrong, &bad, None, &Progress::default())).is_err());
        assert!(!bad.exists());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no .part left behind");

        let stopped = Progress::default();
        stopped.cancel();
        let got = rt.block_on(api.download_to(&hash, &bad, None, &stopped));
        assert_eq!(got.unwrap_err().code, ErrorCode::Cancelled);
        assert!(!bad.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A server on this computer that answers each request with what `reply` makes of its head,
    /// and keeps every head it was sent.
    fn answering(reply: impl Fn(&str) -> Vec<u8> + Send + 'static) -> (String, Arc<parking_lot::Mutex<Vec<String>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let heads = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let seen = heads.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut head = Vec::new();
                let mut byte = [0; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let _ = stream.write_all(&reply(&head));
                seen.lock().push(head);
            }
        });
        (base, heads)
    }

    #[test]
    fn a_download_bigger_than_it_can_be_is_dropped_as_it_comes() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let body = vec![7u8; 300_000];
        // Said up front, and not said at all (the body runs until the connection closes).
        let told = {
            let b = body.clone();
            answering(move |_| {
                [format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len()).into_bytes(), b.clone()].concat()
            })
        };
        let untold = {
            let b = body.clone();
            answering(move |_| [b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec(), b.clone()].concat())
        };
        for (base, _) in [told, untold] {
            let api = Api::new();
            api.set_server(&base, "");
            assert_eq!(rt.block_on(api.file("/f", 100_000)).unwrap_err().code, ErrorCode::FileTooLarge);
            assert_eq!(rt.block_on(api.file("/f", 300_000)).unwrap().0.len(), 300_000);
        }
        let api = Api::new();
        assert_eq!(api.file_cap(), MOST_FILE_BYTES + SEALING, "nothing known yet: the most a server allows");
        api.note_file_limits(&ServerInfo { max_file_bytes: Some(25 << 20), max_file_limit: Some(1 << 30), ..Default::default() });
        assert_eq!(api.file_cap(), (1 << 30) + SEALING, "an owner may have lowered the limit since a bigger file was sent");
    }

    #[test]
    fn the_server_password_does_not_follow_a_redirect_to_another_host() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let health = br#"{"ok":true,"passwordRequired":true,"authenticated":true}"#;
        let ok = move |_: &str| {
            [format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", health.len()).into_bytes(), health.to_vec()]
                .concat()
        };
        let (elsewhere, heard) = answering(ok);
        let away = elsewhere.clone();
        let (base, _) = answering(move |head| {
            let to = if head.starts_with("GET /api/health") { format!("{away}/api/health") } else { "/api/health".into() };
            format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
        });
        let api = Api::new();
        api.set_server(&base, "the door");
        let got = rt.block_on(api.health());
        assert_eq!(got.unwrap_err().status, 302, "not followed");
        assert!(heard.lock().is_empty(), "the other host never heard from it");

        let from = url::Url::parse("http://pi.local:8080/api/x").unwrap();
        let to = |u: &str| url::Url::parse(u).unwrap();
        assert!(may_follow(&from, &to("http://pi.local:8080/api/y")));
        assert!(may_follow(&from, &to("https://PI.local/api/x")), "the same host, moving up to HTTPS");
        assert!(!may_follow(&from, &to("http://pi.local:9000/api/x")), "another port is another server");
        assert!(!may_follow(&from, &to("http://evil.example/api/x")));
        assert!(!may_follow(&to("https://h.example.com"), &to("http://h.example.com")), "never down to plain HTTP");
    }

    #[test]
    fn the_clients_are_made_with_rustls_and_the_system_s_checks() {
        // Building them would panic on a TLS configuration reqwest did not take, or no provider.
        let _ = Api::new();
        install_tls();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
        assert_eq!(tls(&[b"http/1.1"]).alpn_protocols, [b"http/1.1".to_vec()]);
        let _ = reqwest::Client::builder().build().expect("a plain client, as the updater makes, finds the provider");
    }

    /// Over the network, so not run by default: `cargo test -- --ignored https`.
    #[test]
    #[ignore]
    fn https_reaches_a_real_server_and_refuses_a_bad_certificate() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let api = Api::new();
        api.set_server("https://api.github.com", "");
        let got = rt.block_on(api.http.get("https://api.github.com/zen").send()).expect("github over rustls");
        assert!(got.status().is_success() || got.status().as_u16() == 403, "{}", got.status());
        assert!(
            rt.block_on(api.http.get("https://self-signed.badssl.com/").send()).is_err(),
            "a self-signed certificate is refused, as before"
        );
        assert!(rt.block_on(api.http.get("https://expired.badssl.com/").send()).is_err());
        let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls(&[b"http/1.1"])));
        let ws =
            rt.block_on(tokio_tungstenite::connect_async_tls_with_config("wss://ws.postman-echo.com/raw", None, false, Some(connector)));
        assert!(ws.is_ok(), "a socket over the same TLS: {:?}", ws.err());
    }

    #[test]
    fn bases_get_a_scheme_and_lose_trailing_slashes() {
        assert_eq!(normalize_base("pi.local:8080/"), "http://pi.local:8080");
        assert_eq!(normalize_base(" https://h.example.com// "), "https://h.example.com");
    }

    #[test]
    fn only_plain_http_across_the_internet_is_in_the_clear() {
        for open in ["ciranda.example.com:8080", "http://203.0.113.7:8080", "http://[2001:db8::1]:8080", "100.128.0.1"] {
            assert!(sends_in_the_clear(open), "{open}");
        }
        for fine in [
            "https://ciranda.example.com",
            "pi.local:8080",
            "rockpi:8080",
            "localhost:8080",
            "127.0.0.1:8080",
            "192.168.0.20:8080",
            "10.1.2.3",
            "172.20.0.5",
            "169.254.1.1",
            "100.64.0.1",
            "100.127.255.254",
            "box.tail1234.ts.net",
            "http://[::1]:8080",
            "http://[fd7a:115c:a1e0::1]:8080",
            "",
        ] {
            assert!(!sends_in_the_clear(fine), "{fine}");
        }
    }
}
