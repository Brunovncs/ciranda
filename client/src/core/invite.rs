//! Invitations: a server's address and its door password in one link to send a friend, so they
//! only have to choose a name and a password. The link opens a page (the `site/convite` of this
//! repository, on GitHub Pages) that offers the app or its download; the page then hands the same
//! fields to the app as a `ciranda://join` link. Both carry them in the part a browser never sends
//! anywhere (the page's `#`), so the password reaches no server on its way, GitHub's included.
//!
//! Whoever has the link has the door password, as whoever was told it in a message already did;
//! changing the password on the server is what takes it back.

use super::api::normalize_base;

/// The invitation page.
pub const PAGE: &str = "https://brunovncs.github.io/ciranda/convite/";
/// What the page opens, for an app that is installed.
const APP: &str = "ciranda://join";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    /// The address with its scheme, as `normalize_base` makes it.
    pub server: String,
    /// The door password; empty for a server without one.
    pub password: String,
    /// The server's name as the inviter saw it, to say where the link leads.
    pub name: String,
}

fn fields(i: &Invite) -> String {
    let mut f = url::form_urlencoded::Serializer::new(String::new());
    f.append_pair("s", &i.server);
    if !i.password.is_empty() {
        f.append_pair("p", &i.password);
    }
    if !i.name.is_empty() {
        f.append_pair("n", &i.name);
    }
    f.finish()
}

impl Invite {
    pub fn new(server: &str, password: &str, name: &str) -> Invite {
        Invite { server: normalize_base(server), password: password.to_string(), name: name.trim().to_string() }
    }

    /// The link to send: the page, with everything after its `#`.
    pub fn link(&self) -> String {
        format!("{PAGE}#{}", fields(self))
    }

    /// The same, for the app itself. The page makes it; this is what it makes.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn app_link(&self) -> String {
        format!("{APP}?{}", fields(self))
    }

    /// An invitation in whatever was pasted or opened: the page's link or the app's, and nothing
    /// else, so an address that happens to have a `#` is left alone.
    pub fn parse(text: &str) -> Option<Invite> {
        let text = text.trim();
        let query = if let Some(rest) = text.strip_prefix(APP) {
            rest.strip_prefix('?')?
        } else {
            let page = PAGE.trim_end_matches('/');
            let rest = text.strip_prefix(page)?;
            rest.trim_start_matches('/').strip_prefix('#')?
        };
        let (mut server, mut password, mut name) = (None, String::new(), String::new());
        for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
            match &*k {
                "s" => server = Some(v.trim().to_string()),
                "p" => password = v.into_owned(),
                "n" => name = v.trim().to_string(),
                _ => {}
            }
        }
        let server = server.filter(|s| !s.is_empty())?;
        Some(Invite { server: normalize_base(&server), password, name })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_comes_back_as_it_went() {
        let i = Invite::new("https://bloquinho.duckdns.org:8444/", "pão & café #1", "Bloquinho");
        assert_eq!(i.server, "https://bloquinho.duckdns.org:8444");
        let link = i.link();
        assert!(link.starts_with("https://brunovncs.github.io/ciranda/convite/#s=https"), "{link}");
        // Everything is after the #, which the browser keeps to itself.
        assert!(!link.split_once('#').unwrap().0.contains("caf"));
        assert_eq!(Invite::parse(&link), Some(i.clone()));
        assert_eq!(Invite::parse(&format!("  {}\n", i.app_link())), Some(i));
    }

    #[test]
    fn a_server_without_a_password_or_a_name() {
        let i = Invite::new("10.0.0.5:8080", "", "");
        assert_eq!(i.link(), "https://brunovncs.github.io/ciranda/convite/#s=http%3A%2F%2F10.0.0.5%3A8080");
        assert_eq!(
            Invite::parse(&i.link()).unwrap(),
            Invite { server: "http://10.0.0.5:8080".into(), password: "".into(), name: "".into() }
        );
    }

    #[test]
    fn only_invitations_are_taken_for_one() {
        assert_eq!(Invite::parse("bloquinho.duckdns.org:8444"), None);
        assert_eq!(Invite::parse("https://example.com/#s=x"), None);
        assert_eq!(Invite::parse("https://brunovncs.github.io/ciranda/convite/"), None);
        assert_eq!(Invite::parse("https://brunovncs.github.io/ciranda/convite/#p=only-a-password"), None);
        // Without the page's trailing slash, as some apps shorten it.
        let got = Invite::parse("https://brunovncs.github.io/ciranda/convite#s=pi.local%3A8080&n=Casa").unwrap();
        assert_eq!((got.server.as_str(), got.name.as_str()), ("http://pi.local:8080", "Casa"));
    }
}
