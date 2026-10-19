//! Every session the window holds: the server on screen, and the other servers of the account in
//! use, kept in the background for their private messages (see `Session::start_background`). The
//! root keeps this in step with the rail; the views read it to list and open conversations from
//! any of them.

use crate::core::api::normalize_base;
use crate::session::Session;
use gpui::{App, Entity, EntityId, Global};
use std::collections::HashSet;

/// A server kept in the background, and who you are on it.
pub struct Away {
    pub session: Entity<Session>,
    pub url: String,
    pub username: String,
    /// The server of the call you are in, kept whole behind the one on screen (see
    /// `Root::send_backstage`): its full socket and its view, not a background session's.
    pub held: bool,
}

#[derive(Default)]
pub struct Elsewhere {
    /// The server on screen.
    pub home: Option<Entity<Session>>,
    pub away: Vec<Away>,
    /// Being asked whether they can be kept in the background, by `key`.
    pub starting: HashSet<(String, String)>,
    /// Said no (a server without the inbox): not asked again until Ciranda starts again.
    pub unsupported: HashSet<(String, String)>,
}

impl Global for Elsewhere {}

/// How a server and a username are told apart, however the address was typed.
pub fn key(url: &str, username: &str) -> (String, String) {
    (normalize_base(url).to_lowercase(), username.to_string())
}

pub fn elsewhere(cx: &App) -> Option<&Elsewhere> {
    cx.try_global::<Elsewhere>()
}

pub fn elsewhere_mut(cx: &mut App) -> &mut Elsewhere {
    cx.default_global::<Elsewhere>()
}

/// The server on screen first, then the others.
pub fn sessions(cx: &App) -> Vec<Entity<Session>> {
    let Some(e) = elsewhere(cx) else { return Vec::new() };
    e.home.iter().cloned().chain(e.away.iter().map(|a| a.session.clone())).collect()
}

/// A background session by its entity id, while it is still kept.
pub fn away_session(id: EntityId, cx: &App) -> Option<Entity<Session>> {
    elsewhere(cx)?.away.iter().find(|a| a.session.entity_id() == id).map(|a| a.session.clone())
}

/// The background session of this server and username; not the one held for a call, which is
/// brought back whole instead.
pub fn away_for(url: &str, username: &str, cx: &App) -> Option<Entity<Session>> {
    let want = key(url, username);
    elsewhere(cx)?.away.iter().find(|a| !a.held && key(&a.url, &a.username) == want).map(|a| a.session.clone())
}

#[cfg(test)]
mod tests {
    use super::key;

    #[test]
    fn a_server_is_the_same_however_its_address_was_typed() {
        assert_eq!(key("Bloquinho.DuckDNS.org:8444/", "bruno"), key("http://bloquinho.duckdns.org:8444", "bruno"));
        assert_ne!(key("https://pi.local", "bruno"), key("https://pi.local", "pedro"));
        assert_ne!(key("https://pi.local", "bruno"), key("http://pi.local", "bruno"));
    }
}
