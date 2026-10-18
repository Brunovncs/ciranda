//! Private conversations on this computer: whose keys are whose, whether this computer can open
//! your messages, and turning sealed messages into ones the chat can draw (and back).
//!
//! Plain logic over `core::crypto` and `core::keystore`, with no networking and no UI, so the
//! session drives it and the tests can too.
//!
//! The server is not trusted with any of it. It says which key is whose, and it may lie: a key is
//! only used for the person it is listed for, and one of theirs this computer has not accepted
//! before is marked (and never opens a call, nor gets one). It numbers the messages, and it may
//! replay or shuffle them: a sealed text, or a message's own sealed id, seen under two message
//! ids is refused, and an earlier text of a message put back after a newer one was seen is not shown.

use crate::core::api::Sealed;
use crate::core::crypto::{self, Identity, PairKey, RecoveryKey, aad};
use crate::core::keystore::KeyFile;
use crate::core::types::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use zeroize::Zeroizing;

/// Whether this computer can take part in private conversations yet.
#[derive(Clone, Debug, PartialEq)]
pub enum KeyState {
    /// Asking the server what keys there are.
    Loading,
    /// This computer has the account's key: everything works.
    Ready,
    /// The account has a key that this computer does not: the recovery key opens it here.
    Locked,
    /// The server could not be asked; tried again on the next connect.
    Failed(String),
    /// The server is older than private conversations.
    Unsupported,
}

/// What to do after the server said what keys there are.
#[derive(Debug, PartialEq)]
pub enum Next {
    Nothing,
    /// The account has no key at all yet: make one and publish it.
    Create,
}

/// What a sealed message says inside.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Payload {
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<SealedFile>,
    /// The message this answers, with a preview of it: the server can't make one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<ReplyTo>,
    /// The message's own random id, the same across its edits. The server can't change it without
    /// the message failing to open, so one seen under two message ids was sent twice or swapped.
    /// Sealed in the text and not in the associated data: the server numbers a message only after
    /// it is sealed, and 4.3.6, which ignores fields it doesn't know, still opens it this way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mid: Option<String>,
    /// How many times it was edited, as the computer that edited it last knew: which of two texts
    /// is the newer when the server says nothing to tell them apart.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rev: u32,
    /// For an edit: the id the server gave the message, so the new text can't be put on another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of: Option<MessageId>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Why something could not be sealed for the other person.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SealError {
    /// This computer can't seal anything yet: no key, or locked.
    NoKey,
    /// The other person has no key yet.
    PeerHasNoKey,
    /// Their key changed and the change was not accepted here: nothing is sealed to it, neither a
    /// message nor a call's key, until it is.
    KeyNotConfirmed,
}

impl SealError {
    pub fn code(self) -> ErrorCode {
        match self {
            SealError::NoKey => ErrorCode::NoKey,
            SealError::PeerHasNoKey => ErrorCode::PeerHasNoKey,
            SealError::KeyNotConfirmed => ErrorCode::KeyNotConfirmed,
        }
    }

    /// What to tell the person, where the server's words for the code would not say it.
    pub fn message(self) -> Option<&'static str> {
        match self {
            SealError::KeyNotConfirmed => Some(tr!(
                "Their security key changed, so nothing was sent. Compare your safety number or accept the new key in the conversation first.",
                "A chave de segurança dessa pessoa mudou, então nada foi enviado. Comparem o código de segurança ou aceite a nova chave na conversa primeiro."
            )),
            _ => None,
        }
    }
}

/// Where an opened text stands among the texts seen of its message.
enum Placement {
    /// The one to show.
    Current,
    /// An older text than the one shown, arriving late (a message that waited for its key after
    /// its edit had come) or put back: the newer one stays.
    Superseded,
    /// Not where it was sealed to be: under another message's id, or an edit of another message.
    Refused,
}

/// What was seen of one message's texts.
#[derive(Default)]
struct Texts {
    /// Every sealed text of it seen, by nonce: only the sender can make a new one.
    seen: Vec<[u8; 24]>,
    current: Option<[u8; 24]>,
    /// When the current one was written, as the server says, and its edit count.
    edited: i64,
    rev: u32,
    shown: Option<Message>,
}

pub struct Vault {
    pub state: KeyState,
    me: UserId,
    identity: Option<(KeyId, Identity)>,
    /// The account's key as the server has it, with its wrapped private half: what the recovery
    /// key opens on a computer that does not have it yet.
    on_server: Option<OwnKey>,
    file: KeyFile,
    /// Every public key seen, by id: history needs keys since replaced.
    keys: HashMap<KeyId, PublicKeyInfo>,
    /// Each person's current key.
    current: HashMap<UserId, KeyId>,
    /// Agreed keys, by (conversation, my key, their key): an X25519 agreement per message would be
    /// wasted work for the same pair every time.
    pairs: HashMap<(ConversationId, KeyId, KeyId), Rc<PairKey>>,
    /// By person: ids of keys of theirs that something opened with here, not accepted for them.
    unconfirmed: HashMap<UserId, HashSet<KeyId>>,
    /// The message id each sealed text (by its nonce) and each message's own id was seen at.
    nonces: HashMap<[u8; 24], MessageId>,
    mids: HashMap<String, MessageId>,
    texts: HashMap<MessageId, Texts>,
    /// The call (conversation, start) each sealed call key was first opened for.
    call_keys: HashMap<[u8; 24], (ConversationId, i64)>,
}

impl Vault {
    pub fn new(file: KeyFile, me: UserId) -> Vault {
        let identity = file.identity();
        Vault {
            state: KeyState::Loading,
            me,
            identity,
            on_server: None,
            file,
            keys: HashMap::new(),
            current: HashMap::new(),
            pairs: HashMap::new(),
            unconfirmed: HashMap::new(),
            nonces: HashMap::new(),
            mids: HashMap::new(),
            texts: HashMap::new(),
            call_keys: HashMap::new(),
        }
    }

    pub fn ready(&self) -> bool {
        self.state == KeyState::Ready
    }

    pub fn my_key(&self) -> Option<KeyId> {
        self.identity.as_ref().map(|(id, _)| *id)
    }

    /// What the server said about everybody's keys and your own.
    pub fn on_keys(&mut self, reply: KeysReply) -> Next {
        self.learn(reply.keys.iter().cloned());
        for k in &reply.keys {
            if self.owner(k.id) == Some(k.user_id) {
                self.current.insert(k.user_id, k.id);
            }
        }
        match reply.mine {
            MineKey::None => {
                // The server has no key for this account (a new one, or a server that forgot): a
                // new one is made. The one this computer kept is archived, not deleted, so what was
                // sealed to it still opens here.
                if self.identity.is_some() {
                    self.identity = None;
                    self.file.retire_identity();
                }
                self.on_server = None;
                Next::Create
            }
            MineKey::Key(mine) => {
                self.state = match &self.identity {
                    Some((id, identity)) if *id == mine.id && identity.public_b64() == mine.public_key => KeyState::Ready,
                    None if self.file.identity_locked() => KeyState::Failed(
                        tr!(
                            "The system keychain didn't open, so your private-message key can't be read. Unlock it and open Ciranda again.",
                            "O chaveiro do sistema não abriu, então a chave das conversas privadas não pode ser lida. Desbloqueie e abra Ciranda de novo."
                        )
                        .into(),
                    ),
                    _ => KeyState::Locked,
                };
                self.on_server = Some(mine);
                Next::Nothing
            }
            MineKey::Unclear => {
                // An answer that says nothing about your key is not an answer that you have none:
                // keep it, and use it if the directory agrees it is the current one.
                let mine = self.current.get(&self.me).and_then(|id| self.keys.get(id));
                self.state = match (&self.identity, mine) {
                    (Some((id, identity)), Some(k)) if *id == k.id && identity.public_b64() == k.public_key => KeyState::Ready,
                    _ => KeyState::Failed(
                        tr!(
                            "The server's answer about your key didn't make sense. Trying again on the next connection.",
                            "A resposta do servidor sobre a sua chave não fez sentido. Tentando de novo na próxima conexão."
                        )
                        .into(),
                    ),
                };
                Next::Nothing
            }
        }
    }

    /// Makes a new identity key and its recovery key: the wrapping to publish, and the key to show.
    pub fn create() -> (Identity, RecoveryKey, String) {
        let identity = Identity::generate();
        let recovery = RecoveryKey::generate();
        let wrapped = crypto::wrap(&identity, &recovery);
        (identity, recovery, wrapped)
    }

    /// The server took a key this computer made: it is the account's key from now on.
    pub fn adopt(&mut self, key: OwnKey, identity: Identity, recovery: &RecoveryKey, saved: bool) {
        self.file.set_identity(key.id, &identity);
        self.file.set_recovery(recovery, saved);
        self.learn([PublicKeyInfo { id: key.id, user_id: self.me, public_key: key.public_key.clone(), created_at: 0 }]);
        self.current.insert(self.me, key.id);
        self.identity = Some((key.id, identity));
        self.on_server = Some(key);
        self.state = KeyState::Ready;
    }

    /// Opens the account's key with the recovery key typed in. False if it is not the right one.
    pub fn unlock(&mut self, typed: &str) -> bool {
        let (Some(recovery), Some(mine)) = (RecoveryKey::parse(typed), self.on_server.clone()) else { return false };
        let Some(public) = crypto::public_key(&mine.public_key) else { return false };
        let Some(identity) = crypto::unwrap(&mine.wrapped, &public, &recovery) else { return false };
        self.adopt(mine, identity, &recovery, true);
        true
    }

    /// The recovery key, as shown, while this computer keeps it: from when it is made until the
    /// person says it is somewhere safe. Opening it asks the system to unseal it, so it is called
    /// to show it and not to draw a button (`has_recovery`).
    pub fn recovery(&self) -> Option<Zeroizing<String>> {
        self.file.recovery().map(|r| Zeroizing::new(r.display()))
    }

    /// Whether `recovery` has anything to show, without opening it.
    pub fn has_recovery(&self) -> bool {
        self.file.has_recovery()
    }

    /// A recovery key was made here and nobody has said it is kept somewhere yet.
    pub fn needs_backup(&self) -> bool {
        self.ready() && !self.file.recovery_saved()
    }

    /// They kept it: this computer's copy goes, and it can't be shown again. A new one can be made
    /// while the identity key is here (`new_recovery`).
    pub fn mark_backed_up(&mut self) {
        self.file.mark_recovery_saved();
    }

    /// A new recovery key for the key this computer has, and its wrapping to send to the server.
    pub fn new_recovery(&self) -> Option<(KeyId, RecoveryKey, String)> {
        let (id, identity) = self.identity.as_ref()?;
        let recovery = RecoveryKey::generate();
        let wrapped = crypto::wrap(identity, &recovery);
        Some((*id, recovery, wrapped))
    }

    pub fn set_recovery(&mut self, recovery: &RecoveryKey, saved: bool) {
        self.file.set_recovery(recovery, saved);
    }

    // The key directory.

    /// Public keys from anywhere the server sends them. A key is a row that never changes, so an
    /// id sent again with something else in it is not believed: the first answer stands.
    pub fn learn(&mut self, keys: impl IntoIterator<Item = PublicKeyInfo>) {
        for k in keys {
            let (user, id) = (k.user_id, k.id);
            if self.keys.contains_key(&id) {
                continue;
            }
            self.keys.insert(id, k);
            // Key ids only grow: one of theirs newer than the one thought current (met in a message
            // before the push that announces it) is their current one, and is asked about as such.
            if self.current.get(&user).is_some_and(|c| *c < id) {
                self.current.insert(user, id);
            }
        }
    }

    fn owner(&self, key: KeyId) -> Option<UserId> {
        self.keys.get(&key).map(|k| k.user_id)
    }

    /// Somebody published a new key (a `keys:changed` push).
    pub fn key_changed(&mut self, key: PublicKeyInfo) {
        let (user, id) = (key.user_id, key.id);
        self.learn([key]);
        if self.owner(id) == Some(user) && self.current.get(&user).is_none_or(|c| *c < id) {
            self.current.insert(user, id);
        }
        // Your own account has a new key, made on another computer: this one's is spent.
        if user == self.me && self.my_key() != Some(id) {
            self.state = KeyState::Locked;
        }
    }

    /// The key ids among these that are not known yet.
    pub fn unknown(&self, ids: impl IntoIterator<Item = KeyId>) -> Vec<KeyId> {
        let mut out: Vec<KeyId> = ids.into_iter().filter(|id| !self.keys.contains_key(id)).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn peer_has_key(&self, peer: UserId) -> bool {
        self.current.contains_key(&peer)
    }

    fn public_of(&self, key: KeyId) -> Option<[u8; 32]> {
        crypto::public_key(&self.keys.get(&key)?.public_key)
    }

    fn pair(&mut self, conversation: ConversationId, mine: KeyId, peer: UserId, theirs: KeyId) -> Option<Rc<PairKey>> {
        // Their half has to be a key listed as theirs. Otherwise any key the server published, under
        // any id, would open as the other person's (or, paired with yours, as your own words).
        if peer == self.me || self.owner(theirs) != Some(peer) {
            return None;
        }
        if let Some(k) = self.pairs.get(&(conversation, mine, theirs)) {
            return Some(k.clone());
        }
        let public = self.public_of(theirs)?;
        let archived;
        let identity = match &self.identity {
            Some((id, identity)) if *id == mine => identity,
            _ => {
                archived = self.file.archived_identity(mine)?;
                &archived
            }
        };
        let key = Rc::new(crypto::pair_key(identity, self.me, &public, peer, conversation)?);
        self.pairs.insert((conversation, mine, theirs), key.clone());
        Some(key)
    }

    /// Seals something for the other person, to both your current keys, whoever holds theirs.
    /// Everything that leaves goes through `seal_confirmed` instead.
    fn seal(&mut self, conversation: ConversationId, peer: UserId, aad: &str, plain: &[u8]) -> Result<Sealed, SealError> {
        let mine = self.my_key().filter(|_| self.ready()).ok_or(SealError::NoKey)?;
        let theirs = *self.current.get(&peer).ok_or(SealError::PeerHasNoKey)?;
        let key = self.pair(conversation, mine, peer, theirs).ok_or(SealError::PeerHasNoKey)?;
        Ok(Sealed { text: key.seal(aad, plain), sender_key: mine, recipient_key: theirs })
    }

    /// Seals for the other person, but never to a key of theirs not accepted here. The server
    /// says which key is theirs, and whoever holds a key it named reads what is sealed to it: so
    /// the first one met is pinned and taken (as when the conversation starts), and a change after
    /// it waits for the person to accept it.
    fn seal_confirmed(&mut self, conversation: ConversationId, peer: UserId, aad: &str, plain: &[u8]) -> Result<Sealed, SealError> {
        self.my_key().filter(|_| self.ready()).ok_or(SealError::NoKey)?;
        self.pin(peer);
        let theirs = *self.current.get(&peer).ok_or(SealError::PeerHasNoKey)?;
        if !self.is_accepted(peer, theirs) {
            return Err(SealError::KeyNotConfirmed);
        }
        self.seal(conversation, peer, aad, plain)
    }

    /// Opens something sealed between you and `peer`; `from_me` says which of the two keys is yours.
    /// Also says whether their key is one accepted for them here.
    fn open(
        &mut self,
        conversation: ConversationId,
        peer: UserId,
        (sender_key, recipient_key): (KeyId, KeyId),
        from_me: bool,
        aad: &str,
        sealed: &str,
    ) -> Option<(Vec<u8>, bool)> {
        let (mine, theirs) = if from_me { (sender_key, recipient_key) } else { (recipient_key, sender_key) };
        let plain = self.pair(conversation, mine, peer, theirs)?.open(aad, sealed)?;
        Some((plain, self.confirm(peer, theirs)))
    }

    /// Whether `theirs` is a key accepted for `peer` here, first pinning their current one if none
    /// was yet. One that isn't is remembered, so the conversation can say their key changed. Either
    /// of the two people holding the halves of a pair can seal anything in it, so a key nobody
    /// accepted is as good as a stranger's.
    fn confirm(&mut self, peer: UserId, theirs: KeyId) -> bool {
        if self.file.pin(peer).is_none() {
            self.accept_up_to(peer, self.current.get(&peer).copied().unwrap_or(theirs));
        }
        if self.is_accepted(peer, theirs) {
            return true;
        }
        self.unconfirmed.entry(peer).or_default().insert(theirs);
        false
    }

    pub fn seal_message(&mut self, conversation: ConversationId, peer: UserId, payload: &Payload) -> Result<Sealed, SealError> {
        let mut payload = payload.clone();
        // A new message gets an id of its own; an edit keeps the one its message has.
        if payload.mid.is_none() {
            payload.mid = Some(crypto::b64(&crypto::random::<16>()));
        }
        let json = crypto::pad(serde_json::to_vec(&payload).unwrap_or_default());
        self.seal_confirmed(conversation, peer, &aad::message(conversation, self.me), &json)
    }

    pub fn seal_reactions(
        &mut self,
        conversation: ConversationId,
        peer: UserId,
        message: MessageId,
        emoji: &[String],
    ) -> Result<Sealed, SealError> {
        let json = crypto::pad(serde_json::to_vec(emoji).unwrap_or_default());
        self.seal_confirmed(conversation, peer, &aad::reaction(conversation, message, self.me), &json)
    }

    /// A call's media key, sealed by the caller: whoever holds the key it is sealed to hears the call.
    pub fn seal_call_key(&mut self, conversation: ConversationId, peer: UserId, key: &[u8; 32]) -> Result<Sealed, SealError> {
        self.seal_confirmed(conversation, peer, &aad::call(conversation, self.me), key)
    }

    /// The media key of `call`, if it opens and belongs to it. Refused rather than marked when it
    /// is sealed with a key not accepted here: whoever made the key hears the call.
    pub fn open_call_key(&mut self, call: &CallInfo) -> Option<[u8; 32]> {
        let from_me = call.caller_id == self.me;
        let peer = if from_me { call.callee_id } else { call.caller_id };
        let (plain, confirmed) = self.open(
            call.conversation_id,
            peer,
            (call.sender_key, call.recipient_key),
            from_me,
            &aad::call(call.conversation_id, call.caller_id),
            &call.sealed_key,
        )?;
        let key: [u8; 32] = plain.try_into().ok().filter(|_| confirmed)?;
        // The sealed text's nonce is new with every sealing, so it names this call's key: seen
        // before for another call, it is that call's key rung again.
        let nonce = crypto::nonce_of(&call.sealed_key)?;
        let this = (call.conversation_id, call.started_at);
        (*self.call_keys.entry(nonce).or_insert(this) == this).then_some(key)
    }

    /// A sealed message as the chat draws it. One that doesn't open here still shows, saying so.
    pub fn open_message(&mut self, m: &SealedMessage, peer: UserId) -> Message {
        let from_me = m.user_id == self.me;
        let mut private = Private::default();
        let mut body = String::new();
        let mut reply = None;
        let mut current = false;
        if m.kind == "call" {
            private.call = m.meta.clone();
        } else {
            let opened = match (m.sender_key, m.recipient_key, &m.sealed) {
                (Some(s), Some(r), Some(sealed)) => self
                    .open(m.conversation_id, peer, (s, r), from_me, &aad::message(m.conversation_id, m.user_id), sealed)
                    .and_then(|(plain, confirmed)| Some((serde_json::from_slice::<Payload>(&plain).ok()?, confirmed, sealed))),
                _ => None,
            };
            match opened {
                Some((p, confirmed, sealed)) => {
                    let placed = self.place(m, sealed, &p);
                    if let Placement::Superseded = placed
                        && let Some(shown) = self.texts.get(&m.id).and_then(|t| t.shown.clone())
                    {
                        return Message { reactions: self.open_reactions(m.conversation_id, peer, m.id, &m.reactions), ..shown };
                    }
                    if let Placement::Refused = placed {
                        private.unreadable = true;
                        private.out_of_place = true;
                    } else {
                        current = true;
                        private.unconfirmed = !confirmed;
                        private.mid = p.mid;
                        private.rev = p.rev;
                        body = p.body;
                        private.file = p.file;
                        reply = p.reply;
                    }
                }
                None => private.unreadable = true,
            }
        }
        let media_type = private.file.as_ref().map(|f| media_type_of(&f.mime));
        let message = Message {
            id: m.id,
            channel_id: m.conversation_id,
            user_id: Some(m.user_id),
            nickname: None,
            body,
            attachment_hash: m.attachment_hash.clone().filter(|_| private.file.is_some()),
            attachment_name: private.file.as_ref().map(|f| f.name.clone()),
            attachment_bytes: private.file.as_ref().map(|f| f.size),
            media_type,
            pinned: false,
            edited_at: m.edited_at,
            reply,
            reactions: self.open_reactions(m.conversation_id, peer, m.id, &m.reactions),
            mentions: Vec::new(),
            mentions_everyone: false,
            created_at: m.created_at,
            private: Some(private),
        };
        if current && let Some(t) = self.texts.get_mut(&m.id) {
            t.shown = Some(message.clone());
        }
        message
    }

    /// Where an opened text of `m` stands. Refused when its sealed text or its own id was seen under
    /// another message id (sent again, or two swapped), or when it is an edit of another message.
    /// Nothing is remembered of a text that is refused, so it can't spoil the real one.
    ///
    /// A text of this message already seen and since replaced is superseded, not refused: the newer
    /// one stays shown. Only the sender can make a new text, so that is the server putting an old
    /// one back, or a page fetched before an edit arriving after it; either way the old words are
    /// not shown, and the real current ones are not covered by a refusal. A new text older than the
    /// one shown, by the server's edit time and then its edit count, is late, and superseded too.
    ///
    /// What was seen lives as long as the message is kept here (`forget_all_but`): a computer that
    /// never saw the newer text can't tell it was withheld, which is as far as anything the server
    /// keeps can be checked.
    fn place(&mut self, m: &SealedMessage, sealed: &str, p: &Payload) -> Placement {
        let id = m.id;
        let Some(nonce) = crypto::nonce_of(sealed) else { return Placement::Refused };
        let elsewhere = self.nonces.get(&nonce).is_some_and(|at| *at != id)
            || p.mid.as_ref().and_then(|mid| self.mids.get(mid)).is_some_and(|at| *at != id)
            || p.of.is_some_and(|of| of != id);
        let t = self.texts.entry(id).or_default();
        if t.current == Some(nonce) {
            return Placement::Current;
        }
        if elsewhere {
            return Placement::Refused;
        }
        if t.seen.contains(&nonce) {
            return Placement::Superseded;
        }
        let edited = m.edited_at.unwrap_or(0);
        let older = t.current.is_some() && (edited, p.rev) < (t.edited, t.rev);
        t.seen.push(nonce);
        if !older {
            (t.current, t.edited, t.rev) = (Some(nonce), edited, p.rev);
        }
        self.nonces.insert(nonce, id);
        if let Some(mid) = &p.mid {
            self.mids.insert(mid.clone(), id);
        }
        if older { Placement::Superseded } else { Placement::Current }
    }

    /// Forgets what was seen of every message but these: their texts, nonces and own ids. The
    /// session keeps the messages it shows and the last of each conversation, so a replay or a swap
    /// is still told among all of those; one involving a message let go is told again once both
    /// are loaded together.
    pub fn forget_all_but(&mut self, keep: &HashSet<MessageId>) {
        self.texts.retain(|id, _| keep.contains(id));
        self.nonces.retain(|_, id| keep.contains(id));
        self.mids.retain(|_, id| keep.contains(id));
    }

    /// An earlier text of `m`: None if it doesn't open here, or is not one of `m`'s. Seen, it can't
    /// be put back as the current one later.
    pub fn open_version(&mut self, m: &Message, peer: UserId, v: &SealedVersion) -> Option<String> {
        let (conversation, author) = (m.channel_id, m.user_id?);
        let (plain, _) = self.open(
            conversation,
            peer,
            (v.sender_key, v.recipient_key),
            author == self.me,
            &aad::message(conversation, author),
            &v.sealed,
        )?;
        let p = serde_json::from_slice::<Payload>(&plain).ok()?;
        let nonce = crypto::nonce_of(&v.sealed)?;
        let now = m.private.as_ref().and_then(|c| c.mid.as_ref());
        let foreign = self.nonces.get(&nonce).is_some_and(|at| *at != m.id)
            || p.mid.as_ref().and_then(|mid| self.mids.get(mid)).is_some_and(|at| *at != m.id)
            || p.of.is_some_and(|of| of != m.id)
            || p.mid.as_ref().zip(now).is_some_and(|(was, now)| was != now);
        if foreign {
            return None;
        }
        self.nonces.insert(nonce, m.id);
        if let Some(mid) = &p.mid {
            self.mids.insert(mid.clone(), m.id);
        }
        let t = self.texts.entry(m.id).or_default();
        if !t.seen.contains(&nonce) {
            t.seen.push(nonce);
        }
        Some(p.body)
    }

    /// Everybody's sealed reactions to a message, opened and counted as a channel's are.
    pub fn open_reactions(
        &mut self,
        conversation: ConversationId,
        peer: UserId,
        message: MessageId,
        sealed: &[SealedReaction],
    ) -> Vec<Reaction> {
        let mut out: Vec<Reaction> = Vec::new();
        for r in sealed {
            let from_me = r.user_id == self.me;
            let Some((plain, _)) = self.open(
                conversation,
                peer,
                (r.sender_key, r.recipient_key),
                from_me,
                &aad::reaction(conversation, message, r.user_id),
                &r.sealed,
            ) else {
                continue;
            };
            let Ok(emoji) = serde_json::from_slice::<Vec<String>>(&plain) else { continue };
            for e in emoji.into_iter().take(20) {
                match out.iter_mut().find(|x| x.emoji == e) {
                    Some(x) if !x.user_ids.contains(&r.user_id) => {
                        x.count += 1;
                        x.user_ids.push(r.user_id);
                    }
                    Some(_) => {}
                    None => out.push(Reaction { emoji: e, count: 1, user_ids: vec![r.user_id] }),
                }
            }
        }
        out
    }

    // Trust.

    /// The safety number between you and `peer`, with your current keys.
    pub fn safety_number(&self, peer: UserId) -> Option<String> {
        let (_, mine) = self.identity.as_ref()?;
        let theirs = self.public_of(*self.current.get(&peer)?)?;
        Some(crypto::safety_number((self.me, &mine.public()), (peer, &theirs)))
    }

    fn current_public(&self, peer: UserId) -> Option<&str> {
        Some(self.keys.get(self.current.get(&peer)?)?.public_key.as_str())
    }

    /// Remembers `peer`'s key the first time it is seen, so a later change can be noticed.
    pub fn pin(&mut self, peer: UserId) {
        if self.file.pin(peer).is_none()
            && let Some(id) = self.current.get(&peer).copied()
        {
            self.accept_up_to(peer, id);
        }
    }

    /// `peer`'s key `id` is the one to compare theirs against from now on, and the older keys the
    /// directory lists as theirs are accepted with it: what they sealed before a change they made
    /// opens without a warning. Keys that turn up later are not.
    fn accept_up_to(&mut self, peer: UserId, id: KeyId) {
        let older: Vec<String> = self.keys.values().filter(|k| k.user_id == peer && k.id < id).map(|k| k.public_key.clone()).collect();
        self.file.accept(peer, &older.iter().map(String::as_str).collect::<Vec<_>>());
        if let Some(k) = self.keys.get(&id).filter(|k| k.user_id == peer).map(|k| k.public_key.clone()) {
            self.file.set_pin(peer, &k);
        }
    }

    /// Whether `key` is one of `peer`'s accepted here.
    fn is_accepted(&self, peer: UserId, key: KeyId) -> bool {
        let Some(k) = self.keys.get(&key).filter(|k| k.user_id == peer) else { return false };
        if self.file.accepted(peer, &k.public_key) {
            return true;
        }
        // Pinned by 4.3.6, which opened every key of theirs up to the pinned one without a word:
        // those stay accepted, or every change already accepted there would be asked about again.
        self.file
            .upgraded_pin(peer)
            .and_then(|pin| self.keys.values().find(|k| k.user_id == peer && k.public_key == pin))
            .is_some_and(|pinned| key <= pinned.id)
    }

    /// The id of the key pinned for `peer`, if the directory has it.
    fn pin_id(&self, peer: UserId) -> Option<KeyId> {
        let pin = self.file.pin(peer)?;
        self.keys.values().filter(|k| k.user_id == peer && k.public_key == pin).map(|k| k.id).min()
    }

    /// `peer`'s current key is not one accepted for them, or something of theirs opened with a key
    /// between the pinned one and it that isn't. Usually a reinstall without the recovery key; it
    /// is also what somebody in the middle would look like, so it is said, not hidden. A key older
    /// than the pinned one that turns up now was never part of a change of theirs seen here: it is
    /// marked on its messages, and accepting the change does not accept it.
    pub fn key_changed_for(&self, peer: UserId) -> bool {
        if self.file.pin(peer).is_none() {
            return false;
        }
        let Some(current) = self.current.get(&peer).copied() else { return false };
        let since = self.pin_id(peer).unwrap_or(0);
        !self.is_accepted(peer, current)
            || self
                .unconfirmed
                .get(&peer)
                .is_some_and(|seen| seen.iter().any(|k| *k > since && *k < current && !self.is_accepted(peer, *k)))
    }

    /// The change was seen and accepted: their current key, and the ones between the pinned key and
    /// it, which are the change.
    pub fn accept_key(&mut self, peer: UserId) {
        let Some(current) = self.current.get(&peer).copied() else { return };
        let since = self.pin_id(peer).unwrap_or(0);
        let between: Vec<String> =
            self.keys.values().filter(|k| k.user_id == peer && k.id > since && k.id < current).map(|k| k.public_key.clone()).collect();
        self.file.accept(peer, &between.iter().map(String::as_str).collect::<Vec<_>>());
        if let Some(k) = self.keys.get(&current).map(|k| k.public_key.clone()) {
            self.file.set_pin(peer, &k);
            if self.file.verified(peer).is_some_and(|v| v != k) {
                self.file.set_verified(peer, None);
            }
        }
        let seen = self.unconfirmed.remove(&peer).unwrap_or_default();
        let still: HashSet<KeyId> = seen.into_iter().filter(|k| !self.is_accepted(peer, *k)).collect();
        if !still.is_empty() {
            self.unconfirmed.insert(peer, still);
        }
    }

    pub fn verified(&self, peer: UserId) -> bool {
        match (self.file.verified(peer), self.current_public(peer)) {
            (Some(v), Some(now)) => v == now,
            _ => false,
        }
    }

    pub fn set_verified(&mut self, peer: UserId, on: bool) {
        let key = if on { self.current_public(peer).map(str::to_string) } else { None };
        if key.is_some() {
            self.accept_key(peer);
        }
        self.file.set_verified(peer, key.as_deref());
    }
}

/// What a sealed attachment is drawn as, from the type it was sealed with.
pub fn media_type_of(mime: &str) -> MediaType {
    match mime.split('/').next().unwrap_or("") {
        "image" => MediaType::Image,
        "video" => MediaType::Video,
        "audio" => MediaType::Audio,
        _ => MediaType::File,
    }
}

/// A line about a call, as it reads in the conversation.
pub fn call_line(meta: &CallMeta, caller: &str, from_me: bool) -> String {
    match meta.outcome.as_str() {
        "ended" => {
            let secs = meta.duration_ms.unwrap_or(0) / 1000;
            let length = if secs < 60 {
                trf!("{} s", "{} s", secs.max(1))
            } else if secs < 3600 {
                trf!("{} min", "{} min", secs / 60)
            } else {
                trf!("{} h {} min", "{} h {} min", secs / 3600, (secs % 3600) / 60)
            };
            trf!("Call · {}", "Chamada · {}", length)
        }
        "declined" if from_me => tr!("Your call was declined", "Sua chamada foi recusada").into(),
        "declined" => trf!("You declined a call from {}", "Você recusou uma chamada de {}", caller),
        _ if from_me => tr!("Call not answered", "Chamada não atendida").into(),
        _ => trf!("Missed call from {}", "Chamada perdida de {}", caller),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file() -> KeyFile {
        KeyFile::open_at(std::env::temp_dir().join(format!("ciranda-vault-{}.json", hex::encode(crypto::random::<8>()))))
    }

    /// Two people with keys published, and the directory each sees.
    fn pair_up() -> (Vault, Vault) {
        let mut a = Vault::new(temp_file(), 1);
        let mut b = Vault::new(temp_file(), 2);
        assert_eq!(a.on_keys(KeysReply { keys: Vec::new(), mine: MineKey::None }), Next::Create);
        assert_eq!(b.on_keys(KeysReply { keys: Vec::new(), mine: MineKey::None }), Next::Create);
        let (ia, ra, wa) = Vault::create();
        let (ib, rb, wb) = Vault::create();
        let ka = OwnKey { id: 10, public_key: ia.public_b64(), wrapped: wa };
        let kb = OwnKey { id: 20, public_key: ib.public_b64(), wrapped: wb };
        let pa = PublicKeyInfo { id: 10, user_id: 1, public_key: ka.public_key.clone(), created_at: 0 };
        let pb = PublicKeyInfo { id: 20, user_id: 2, public_key: kb.public_key.clone(), created_at: 0 };
        a.adopt(ka, ia, &ra, false);
        b.adopt(kb, ib, &rb, false);
        a.key_changed(pb.clone());
        b.key_changed(pa.clone());
        (a, b)
    }

    /// What `edit_dm` seals for an edit of `m`.
    fn edit_of(m: &Message, body: &str) -> Payload {
        let p = m.private.clone().unwrap_or_default();
        Payload { body: body.into(), reply: m.reply.clone(), mid: p.mid, rev: p.rev + 1, of: Some(m.id), ..Default::default() }
    }

    fn as_sent(sealed: Sealed, id: MessageId, from: UserId) -> SealedMessage {
        SealedMessage {
            id,
            conversation_id: 5,
            user_id: from,
            kind: "text".into(),
            sender_key: Some(sealed.sender_key),
            recipient_key: Some(sealed.recipient_key),
            sealed: Some(sealed.text),
            meta: None,
            attachment_hash: None,
            reactions: Vec::new(),
            created_at: 1,
            edited_at: None,
        }
    }

    #[test]
    fn a_message_opens_on_both_sides() {
        let (mut a, mut b) = pair_up();
        let sealed = a.seal_message(5, 2, &Payload { body: "oi, tudo bem?".into(), ..Default::default() }).unwrap();
        let m = as_sent(sealed, 1, 1);
        assert_eq!(b.open_message(&m, 1).body, "oi, tudo bem?");
        assert_eq!(a.open_message(&m, 2).body, "oi, tudo bem?", "your own message opens too, on any of your computers");
    }

    #[test]
    fn a_message_moved_to_another_conversation_or_sender_does_not_open() {
        let (mut a, mut b) = pair_up();
        let sealed = a.seal_message(5, 2, &Payload { body: "x".into(), ..Default::default() }).unwrap();
        let mut m = as_sent(sealed, 1, 1);
        m.user_id = 2;
        let opened = b.open_message(&m, 1);
        assert!(opened.private.unwrap().unreadable);
        m.user_id = 1;
        m.conversation_id = 6;
        assert!(b.open_message(&m, 1).private.unwrap().unreadable);
    }

    #[test]
    fn a_reply_and_an_earlier_version_open_on_the_other_side() {
        let (mut a, mut b) = pair_up();
        let reply = ReplyTo { id: 7, user_id: Some(2), body: "bora jogar?".into(), attachment_name: None, deleted: false };
        let first = a.seal_message(5, 2, &Payload { body: "bora".into(), reply: Some(reply.clone()), ..Default::default() }).unwrap();
        let v = SealedVersion { sender_key: first.sender_key, recipient_key: first.recipient_key, sealed: first.text.clone(), at: 1 };
        let sent = a.open_message(&as_sent(first, 8, 1), 2);
        let edit = a.seal_message(5, 2, &edit_of(&sent, "bora!")).unwrap();
        let m = b.open_message(&as_sent(edit, 8, 1), 1);
        assert_eq!((m.body.as_str(), m.reply.clone()), ("bora!", Some(reply)));
        assert_eq!(b.open_version(&m, 1, &v).as_deref(), Some("bora"));
        let theirs = Message { user_id: Some(2), ..m.clone() };
        assert_eq!(b.open_version(&theirs, 1, &v), None, "passed off as the other person's, it doesn't open");
    }

    #[test]
    fn reactions_count_up_across_both_people() {
        let (mut a, mut b) = pair_up();
        let ra = a.seal_reactions(5, 2, 9, &["👍".into(), "🔥".into()]).unwrap();
        let rb = b.seal_reactions(5, 1, 9, &["👍".into()]).unwrap();
        let sealed = [
            SealedReaction { user_id: 1, sender_key: ra.sender_key, recipient_key: ra.recipient_key, sealed: ra.text },
            SealedReaction { user_id: 2, sender_key: rb.sender_key, recipient_key: rb.recipient_key, sealed: rb.text },
        ];
        let counted = b.open_reactions(5, 1, 9, &sealed);
        assert_eq!(counted[0], Reaction { emoji: "👍".into(), count: 2, user_ids: vec![1, 2] });
        assert_eq!(counted[1].emoji, "🔥");
        assert!(b.open_reactions(5, 1, 10, &sealed).is_empty(), "moved to another message, they open as nothing");
    }

    #[test]
    fn a_call_key_reaches_the_other_side() {
        let (mut a, mut b) = pair_up();
        let key = crypto::random::<32>();
        let s = a.seal_call_key(5, 2, &key).unwrap();
        let call = CallInfo {
            conversation_id: 5,
            caller_id: 1,
            callee_id: 2,
            state: CallPhase::Ringing,
            started_at: 0,
            answered_at: None,
            sealed_key: s.text,
            sender_key: s.sender_key,
            recipient_key: s.recipient_key,
            outcome: None,
            roster: Vec::new(),
        };
        assert_eq!(b.open_call_key(&call), Some(key));
        assert_eq!(a.open_call_key(&call), Some(key));
    }

    #[test]
    fn another_computer_unlocks_with_the_recovery_key_and_only_with_it() {
        let identity = Identity::generate();
        let recovery = RecoveryKey::generate();
        let mine = OwnKey { id: 3, public_key: identity.public_b64(), wrapped: crypto::wrap(&identity, &recovery) };
        let mut v = Vault::new(temp_file(), 1);
        assert_eq!(v.on_keys(KeysReply { keys: Vec::new(), mine: MineKey::Key(mine) }), Next::Nothing);
        assert_eq!(v.state, KeyState::Locked);
        assert!(!v.unlock("ABCD-EFGH"));
        assert!(!v.unlock(&RecoveryKey::generate().display()));
        assert!(v.unlock(&recovery.display().to_lowercase()));
        assert_eq!(v.state, KeyState::Ready);
        assert!(!v.needs_backup(), "they typed it in, so they have it");
    }

    #[test]
    fn a_new_key_made_elsewhere_locks_this_computer() {
        let (mut a, _) = pair_up();
        a.key_changed(PublicKeyInfo { id: 11, user_id: 1, public_key: Identity::generate().public_b64(), created_at: 0 });
        assert_eq!(a.state, KeyState::Locked);
        assert_eq!(a.seal_message(5, 2, &Payload::default()).unwrap_err(), SealError::NoKey);
    }

    #[test]
    fn a_changed_key_is_noticed_until_accepted() {
        let (mut a, _) = pair_up();
        a.pin(2);
        assert!(!a.key_changed_for(2));
        a.key_changed(PublicKeyInfo { id: 21, user_id: 2, public_key: Identity::generate().public_b64(), created_at: 0 });
        assert!(a.key_changed_for(2));
        a.accept_key(2);
        assert!(!a.key_changed_for(2));
    }

    #[test]
    fn nothing_is_sealed_to_a_changed_key_until_it_is_accepted() {
        let (mut a, _) = pair_up();
        let hello = Payload { body: "oi".into(), ..Default::default() };
        assert_eq!(a.seal_message(5, 2, &hello).unwrap().recipient_key, 20, "first contact: their key is taken");

        // The server names a new key as theirs (a push, or the keys in a `stale_key` refusal).
        a.key_changed(PublicKeyInfo { id: 21, user_id: 2, public_key: Identity::generate().public_b64(), created_at: 0 });
        assert_eq!(a.seal_message(5, 2, &hello).unwrap_err(), SealError::KeyNotConfirmed);
        assert_eq!(a.seal_reactions(5, 2, 9, &["👍".into()]).unwrap_err(), SealError::KeyNotConfirmed);
        assert_eq!(a.seal_call_key(5, 2, &crypto::random::<32>()).unwrap_err(), SealError::KeyNotConfirmed);
        assert_eq!(SealError::KeyNotConfirmed.code(), ErrorCode::KeyNotConfirmed);
        assert!(a.key_changed_for(2), "and the conversation says so");

        a.accept_key(2);
        assert_eq!(a.seal_message(5, 2, &hello).unwrap().recipient_key, 21, "accepted: sealed to the new key");
        assert!(a.seal_reactions(5, 2, 9, &["👍".into()]).is_ok());

        // Marking the safety number as checked accepts a change too.
        a.key_changed(PublicKeyInfo { id: 22, user_id: 2, public_key: Identity::generate().public_b64(), created_at: 0 });
        assert!(a.seal_message(5, 2, &hello).is_err());
        a.set_verified(2, true);
        assert_eq!(a.seal_message(5, 2, &hello).unwrap().recipient_key, 22);
    }

    #[test]
    fn first_contact_is_sealed_without_asking_even_after_they_changed_keys() {
        let (a, _) = pair_up();
        let mut fresh = Vault::new(temp_file(), 1);
        fresh.identity = a.identity.as_ref().map(|(id, _)| (*id, a.file.identity().unwrap().1));
        fresh.state = KeyState::Ready;
        let (older, newer) = (Identity::generate(), Identity::generate());
        fresh.learn([
            PublicKeyInfo { id: 19, user_id: 2, public_key: older.public_b64(), created_at: 0 },
            PublicKeyInfo { id: 23, user_id: 2, public_key: newer.public_b64(), created_at: 0 },
        ]);
        fresh.current.insert(2, 23);
        assert!(fresh.file.pin(2).is_none());
        let sealed = fresh.seal_message(5, 2, &Payload { body: "primeira".into(), ..Default::default() }).unwrap();
        assert_eq!(sealed.recipient_key, 23);
        assert_eq!(fresh.file.pin(2), Some(newer.public_b64().as_str()), "pinned as it was sealed to");
        assert!(!fresh.key_changed_for(2));
    }

    #[test]
    fn what_the_session_lets_go_the_vault_forgets_and_what_it_keeps_stays_checked() {
        let (mut a, mut b) = pair_up();
        let one = a.seal_message(5, 2, &Payload { body: "um".into(), ..Default::default() }).unwrap();
        let two = a.seal_message(5, 2, &Payload { body: "dois".into(), ..Default::default() }).unwrap();
        b.open_message(&as_sent(one.clone(), 1, 1), 1);
        b.open_message(&as_sent(two, 2, 1), 1);
        assert_eq!((b.texts.len(), b.nonces.len(), b.mids.len()), (2, 2, 2));

        b.forget_all_but(&HashSet::from([1]));
        assert_eq!((b.texts.len(), b.nonces.len(), b.mids.len()), (1, 1, 1));
        assert!(b.texts.get(&1).is_some_and(|t| t.shown.is_some()));
        assert!(b.open_message(&as_sent(one.clone(), 3, 1), 1).private.unwrap().out_of_place, "a message kept is still told replayed");

        b.forget_all_but(&HashSet::new());
        assert!(b.texts.is_empty() && b.nonces.is_empty() && b.mids.is_empty());
        assert_eq!(b.open_message(&as_sent(one, 1, 1), 1).body, "um", "loaded again, it opens as before");
    }

    #[test]
    fn nobody_without_a_key_can_be_written_to() {
        let (mut a, _) = pair_up();
        assert_eq!(a.seal_message(5, 99, &Payload::default()).unwrap_err(), SealError::PeerHasNoKey);
    }

    fn call(s: Sealed, caller: UserId, started_at: i64) -> CallInfo {
        CallInfo {
            conversation_id: 5,
            caller_id: caller,
            callee_id: 3 - caller,
            state: CallPhase::Ringing,
            started_at,
            answered_at: None,
            sealed_key: s.text,
            sender_key: s.sender_key,
            recipient_key: s.recipient_key,
            outcome: None,
            roster: Vec::new(),
        }
    }

    /// The server's own key, published as somebody's, and something sealed with it as them to `to`.
    fn forged(to: &Vault, key: KeyId, owner: UserId, aad: &str, plain: &[u8]) -> (PublicKeyInfo, Sealed) {
        let server = Identity::generate();
        let (mine, identity) = to.identity.as_ref().unwrap();
        let pk = crypto::pair_key(&server, 2, &identity.public(), to.me, 5).unwrap();
        let info = PublicKeyInfo { id: key, user_id: owner, public_key: server.public_b64(), created_at: 0 };
        (info, Sealed { text: pk.seal(aad, plain), sender_key: key, recipient_key: *mine })
    }

    fn text(body: &str) -> Vec<u8> {
        serde_json::to_vec(&Payload { body: body.into(), ..Default::default() }).unwrap()
    }

    fn sent_at(sealed: Sealed, id: MessageId, from: UserId, edited: Option<i64>) -> SealedMessage {
        SealedMessage { edited_at: edited, ..as_sent(sealed, id, from) }
    }

    #[test]
    fn a_key_that_is_not_the_peers_does_not_open_as_theirs() {
        let (mut a, _) = pair_up();
        a.pin(2);

        // Listed as somebody else's: refused outright.
        let (eve, sealed) = forged(&a, 30, 3, &aad::message(5, 2), &text("pix pra essa conta"));
        a.learn([eve]);
        assert!(a.open_message(&as_sent(sealed, 1, 2), 2).private.unwrap().unreadable);
        assert!(!a.key_changed_for(2));

        // Listed as theirs, but not one this computer accepted: marked, said, and no call either way.
        let (fake, sealed) = forged(&a, 31, 2, &aad::message(5, 2), &text("pix pra essa conta"));
        a.learn([fake]);
        let m = a.open_message(&as_sent(sealed, 2, 2), 2);
        assert!(m.private.unwrap().unconfirmed);
        assert!(a.key_changed_for(2));
        let (_, s) = forged(&a, 31, 2, &aad::call(5, 2), &crypto::random::<32>());
        assert_eq!(a.open_call_key(&call(s, 2, 1)), None);
        assert_eq!(a.seal_call_key(5, 2, &crypto::random::<32>()).unwrap_err(), SealError::KeyNotConfirmed);
    }

    #[test]
    fn accepting_a_change_does_not_accept_a_key_slipped_into_old_history() {
        let (mut a, _) = pair_up();
        a.pin(2);
        let (old, sealed) = forged(&a, 5, 2, &aad::message(5, 2), &text("antigo"));
        a.learn([old]);
        let slipped = as_sent(sealed, 1, 2);
        assert!(a.open_message(&slipped, 2).private.unwrap().unconfirmed);
        assert!(!a.key_changed_for(2), "older than the pinned key: marked on the message, not a change of theirs");

        let (new, sealed) = forged(&a, 21, 2, &aad::message(5, 2), &text("troquei de pc"));
        a.learn([new]);
        assert!(a.key_changed_for(2));
        a.accept_key(2);
        assert!(!a.key_changed_for(2));
        assert!(!a.open_message(&as_sent(sealed, 2, 2), 2).private.unwrap().unconfirmed);
        assert!(a.open_message(&slipped, 2).private.unwrap().unconfirmed, "still marked");
    }

    #[test]
    fn a_fresh_install_and_an_upgrade_from_4_3_6_ask_about_nothing_already_accepted() {
        let (mut a, mut b) = pair_up();
        let hello = as_sent(b.seal_message(5, 1, &Payload { body: "oi".into(), ..Default::default() }).unwrap(), 1, 2);
        let (newer, _) = forged(&a, 21, 2, "", b"");
        let directory = |v: &mut Vault| {
            v.learn(a.keys.values().cloned().chain([newer.clone()]));
            v.current.insert(2, 21);
        };

        // First contact, after they had changed keys: everything the directory lists as theirs then
        // is accepted with the current one.
        let mut fresh = Vault::new(temp_file(), 1);
        directory(&mut fresh);
        fresh.identity = a.identity.as_ref().map(|(id, _)| (*id, a.file.identity().unwrap().1));
        let m = fresh.open_message(&hello, 2);
        assert_eq!(m.body, "oi");
        assert!(!m.private.unwrap().unconfirmed);
        assert!(!fresh.key_changed_for(2));

        // 4.3.6 pinned their new key after the change was accepted there.
        let path = std::env::temp_dir().join(format!("ciranda-vault-{}.json", hex::encode(crypto::random::<8>())));
        std::fs::write(&path, serde_json::json!({ "pins": { "2": newer.public_key } }).to_string()).unwrap();
        let mut upgraded = Vault::new(KeyFile::open_at(path), 1);
        directory(&mut upgraded);
        upgraded.identity = a.identity.take();
        assert!(!upgraded.open_message(&hello, 2).private.unwrap().unconfirmed);
        assert!(!upgraded.key_changed_for(2));
    }

    #[test]
    fn a_replayed_or_swapped_message_is_refused() {
        let (mut a, mut b) = pair_up();
        let one = a.seal_message(5, 2, &Payload { body: "um".into(), ..Default::default() }).unwrap();
        let two = a.seal_message(5, 2, &Payload { body: "dois".into(), ..Default::default() }).unwrap();
        assert_eq!(b.open_message(&as_sent(one.clone(), 1, 1), 1).body, "um");
        assert_eq!(b.open_message(&as_sent(two.clone(), 2, 1), 1).body, "dois");
        assert_eq!(b.open_message(&as_sent(one.clone(), 1, 1), 1).body, "um", "the same message again is fine");

        let again = b.open_message(&as_sent(one.clone(), 3, 1), 1);
        assert!(again.private.as_ref().unwrap().out_of_place && again.body.is_empty(), "sent again under a new id");
        assert!(b.open_message(&as_sent(two, 1, 1), 1).private.unwrap().out_of_place, "swapped");
        assert!(b.open_message(&as_sent(one, 2, 1), 1).private.unwrap().out_of_place, "swapped");
        let three = a.seal_message(5, 2, &Payload::default()).unwrap();
        assert!(!b.open_message(&as_sent(three, 3, 1), 1).private.unwrap().out_of_place, "a refused text left id 3 free");

        // One sealed before messages had ids: told by its nonce.
        let json = crypto::pad(br#"{"body":"antigo"}"#.to_vec());
        let old = a.seal(5, 2, &aad::message(5, 1), &json).unwrap();
        assert_eq!(b.open_message(&as_sent(old.clone(), 7, 1), 1).body, "antigo");
        assert!(b.open_message(&as_sent(old, 9, 1), 1).private.unwrap().out_of_place);
    }

    #[test]
    fn an_earlier_text_put_back_is_not_shown_and_an_edit_does_not_move() {
        let (mut a, mut b) = pair_up();
        let first = a.seal_message(5, 2, &Payload { body: "5 reais".into(), ..Default::default() }).unwrap();
        let sent = a.open_message(&as_sent(first.clone(), 4, 1), 2);
        let edit = a.seal_message(5, 2, &edit_of(&sent, "50 reais")).unwrap();
        assert_eq!(b.open_message(&sent_at(first.clone(), 4, 1, None), 1).body, "5 reais");
        assert_eq!(b.open_message(&sent_at(edit.clone(), 4, 1, Some(2)), 1).body, "50 reais");
        let back = b.open_message(&sent_at(first.clone(), 4, 1, Some(3)), 1);
        assert_eq!(back.body, "50 reais", "the earlier text put back: the newer one stays");
        assert!(!back.private.unwrap().out_of_place);
        assert_eq!(b.open_message(&sent_at(edit.clone(), 4, 1, Some(2)), 1).body, "50 reais");

        // An earlier text first seen in the edit history can't come back either.
        let again = a.open_message(&sent_at(edit.clone(), 4, 1, Some(2)), 2);
        let third = a.seal_message(5, 2, &edit_of(&again, "500 reais")).unwrap();
        let mut c = Vault::new(temp_file(), 2);
        c.identity = b.identity.take();
        c.state = KeyState::Ready;
        c.learn(b.keys.values().cloned());
        let now = c.open_message(&sent_at(third, 4, 1, Some(4)), 1);
        let v = SealedVersion { sender_key: edit.sender_key, recipient_key: edit.recipient_key, sealed: edit.text.clone(), at: 2 };
        assert_eq!(c.open_version(&now, 1, &v).as_deref(), Some("50 reais"));
        assert_eq!(c.open_message(&sent_at(edit.clone(), 4, 1, Some(5)), 1).body, "500 reais");

        // On a computer that saw none of it, an edit still can't go on another message.
        let mut d = Vault::new(temp_file(), 2);
        d.identity = c.identity.take();
        d.state = KeyState::Ready;
        d.learn(c.keys.values().cloned());
        assert!(d.open_message(&sent_at(edit, 6, 1, Some(2)), 1).private.unwrap().out_of_place);
    }

    #[test]
    fn a_late_original_does_not_replace_its_edit() {
        let (mut a, mut b) = pair_up();
        let first = a.seal_message(5, 2, &Payload { body: "bom dia".into(), ..Default::default() }).unwrap();
        let sent = a.open_message(&as_sent(first.clone(), 4, 1), 2);
        let edit = a.seal_message(5, 2, &edit_of(&sent, "boa tarde")).unwrap();
        assert_eq!(b.open_message(&sent_at(edit, 4, 1, Some(2)), 1).body, "boa tarde");
        let late = b.open_message(&sent_at(first, 4, 1, None), 1);
        assert_eq!(late.body, "boa tarde", "it waited for its key; the edit stays");
        assert!(!late.private.unwrap().out_of_place);
    }

    #[test]
    fn edits_from_4_3_6_or_a_computer_that_missed_one_are_taken() {
        let (mut a, mut b) = pair_up();
        let first = a.seal_message(5, 2, &Payload { body: "v1".into(), ..Default::default() }).unwrap();
        let sent = a.open_message(&as_sent(first.clone(), 4, 1), 2);
        let second = a.seal_message(5, 2, &edit_of(&sent, "v2")).unwrap();
        let seen = a.open_message(&sent_at(second.clone(), 4, 1, Some(2)), 2);
        let third = a.seal_message(5, 2, &edit_of(&seen, "v3")).unwrap();
        assert_eq!(b.open_message(&sent_at(first, 4, 1, None), 1).body, "v1");
        assert_eq!(b.open_message(&sent_at(second, 4, 1, Some(2)), 1).body, "v2");
        assert_eq!(b.open_message(&sent_at(third, 4, 1, Some(3)), 1).body, "v3");

        // Their other computer only saw v1, so its edit counts 1: newer all the same.
        let stale = a.seal_message(5, 2, &edit_of(&sent, "v4")).unwrap();
        assert_eq!(b.open_message(&sent_at(stale, 4, 1, Some(4)), 1).body, "v4");

        // And 4.3.6 seals an edit without an id or a count.
        let json = crypto::pad(br#"{"body":"v5"}"#.to_vec());
        let old = a.seal(5, 2, &aad::message(5, 1), &json).unwrap();
        let m = b.open_message(&sent_at(old, 4, 1, Some(5)), 1);
        assert_eq!(m.body, "v5");
        assert!(!m.private.unwrap().out_of_place);
    }

    #[test]
    fn calls_ring_between_new_clients_whoever_wrote_first() {
        let (mut a, mut b) = pair_up();
        // A wrote; B hasn't written since updating. Either can call the other.
        let hello = a.seal_message(5, 2, &Payload { body: "liga?".into(), ..Default::default() }).unwrap();
        b.open_message(&as_sent(hello, 1, 1), 1);
        let key = crypto::random::<32>();
        let s = a.seal_call_key(5, 2, &key).unwrap();
        assert_eq!(b.open_call_key(&call(s.clone(), 1, 100)), Some(key));
        assert_eq!(b.open_call_key(&call(s.clone(), 1, 100)), Some(key), "the same call again: still its key");
        assert_eq!(b.open_call_key(&call(s, 1, 200)), None, "an old call's key put into another call");
        let back = crypto::random::<32>();
        let s = b.seal_call_key(5, 1, &back).unwrap();
        assert_eq!(a.open_call_key(&call(s, 2, 300)), Some(back));
    }

    #[test]
    fn what_older_versions_sealed_still_opens() {
        let (mut a, mut b) = pair_up();
        // 4.3.6 sealed the payload without an id, and a call key bare, as this version does.
        let json = crypto::pad(br#"{"body":"de antes"}"#.to_vec());
        let m = b.open_message(&as_sent(a.seal(5, 2, &aad::message(5, 1), &json).unwrap(), 1, 1), 1);
        assert_eq!(m.body, "de antes");
        assert_eq!(m.private.unwrap().mid, None);
        let key = crypto::random::<32>();
        let bare = a.seal(5, 2, &aad::call(5, 1), &key).unwrap();
        assert_eq!(b.open_call_key(&call(bare, 1, 1)), Some(key));

        // And what this version seals reads as 4.3.6 reads it: unknown fields are ignored.
        #[derive(Deserialize)]
        struct Old {
            body: String,
        }
        let sealed = a.seal_message(5, 2, &Payload { body: "de agora".into(), ..Default::default() }).unwrap();
        let pk = b.pair(5, 20, 1, 10).unwrap();
        let plain = pk.open(&aad::message(5, 1), &sealed.text).unwrap();
        assert_eq!(serde_json::from_slice::<Old>(&plain).unwrap().body, "de agora");
        let s = a.seal_call_key(5, 2, &key).unwrap();
        assert_eq!(pk.open(&aad::call(5, 1), &s.text).unwrap(), key.to_vec());
    }

    #[test]
    fn an_unclear_answer_keeps_your_key_and_a_null_one_archives_it() {
        let (mut a, mut b) = pair_up();
        let before = b.seal_message(5, 1, &Payload { body: "guardado".into(), ..Default::default() }).unwrap();
        let mine = PublicKeyInfo { id: 10, user_id: 1, public_key: a.identity.as_ref().unwrap().1.public_b64(), created_at: 0 };
        assert_eq!(a.on_keys(KeysReply { keys: vec![mine], mine: MineKey::Unclear }), Next::Nothing);
        assert_eq!(a.my_key(), Some(10));
        assert_eq!(a.state, KeyState::Ready);
        assert_eq!(a.on_keys(KeysReply::default()), Next::Nothing, "a reply without the field at all");
        assert_eq!(a.my_key(), Some(10));

        assert_eq!(a.on_keys(KeysReply { keys: Vec::new(), mine: MineKey::None }), Next::Create);
        assert_eq!(a.my_key(), None);
        let (identity, recovery, wrapped) = Vault::create();
        a.adopt(OwnKey { id: 11, public_key: identity.public_b64(), wrapped }, identity, &recovery, false);
        assert_eq!(a.open_message(&as_sent(before, 1, 2), 2).body, "guardado", "sealed to the old key, still opens here");
    }

    #[test]
    fn call_lines_read_naturally() {
        let ended = CallMeta { outcome: "ended".into(), duration_ms: Some(125_000) };
        assert_eq!(call_line(&ended, "Ana", true), "Call · 2 min");
        assert_eq!(call_line(&CallMeta { outcome: "missed".into(), duration_ms: None }, "Ana", false), "Missed call from Ana");
    }
}
