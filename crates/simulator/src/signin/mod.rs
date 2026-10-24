//! Signing in with another account: Discord, Reddit, Mastodon (and the rest of
//! the fediverse), Codeberg, Google, GitHub, or a community's own OpenID
//! server (`docs/notes/external-sign-in.md`, design in
//! `docs/mockups/external-sign-in/`).
//!
//! **The account stays a Cartopolis account.** The other one only confirms who
//! somebody is; the name, the chats and the buildings are this server's, and
//! an account may have a password, several other accounts, or both — but
//! never none (`store::remove_identity`).
//!
//! **A round trip is three requests and two pieces of state, both in memory.**
//! `start` (or `begin`, for a signed-in caller) remembers a [`Flow`] under a
//! random `state` and sends the browser to the provider; the provider sends it
//! back to `callback` with a code, which is exchanged for the person's profile
//! and turned into a [`Ticket`]; the web UI's `/welcome` page reads the ticket
//! and does what it says — signs in, asks for a name, asks for a password to
//! link. Nothing about a round trip in flight survives a restart, which costs
//! somebody mid-sign-in one more click and saves a table of half-states.
//!
//! **The app pairs instead of being redirected to.** An Android app link does
//! not reach an app that is already running (`platform::android::launch_url`),
//! so the app asks for a [`Pairing`] code, opens the browser with it, and polls
//! for the session the browser's round trip leaves there.

pub mod http;
pub mod keys;
pub mod providers;
pub mod store;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::fediverse::fetch;
use crate::registration::Throttle;

/// How long a round trip, its answer and a pairing code live.
pub const FLOW_TTL: Duration = Duration::from_secs(10 * 60);

/// The protocol family a provider speaks. Google, Codeberg and a community's
/// own server are all plain OpenID Connect; the other four each need a few
/// lines of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Discord,
    Reddit,
    Mastodon,
    Github,
    Oidc,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discord => "discord",
            Self::Reddit => "reddit",
            Self::Mastodon => "mastodon",
            Self::Github => "github",
            Self::Oidc => "oidc",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "discord" => Self::Discord,
            "reddit" => Self::Reddit,
            "mastodon" => Self::Mastodon,
            "github" => Self::Github,
            "oidc" => Self::Oidc,
            _ => return None,
        })
    }

    /// Whether a community may be linked to a group here: a Discord server or
    /// a subreddit. The others have no group whose members we could ask about.
    #[must_use]
    pub fn has_groups(self) -> bool {
        matches!(self, Self::Discord | Self::Reddit)
    }
}

/// Who may make a new account by signing in with a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewAccounts {
    Anyone,
    /// Only people in a Discord server or subreddit linked to a community:
    /// a server can open itself to the groups it chose and nobody else.
    Members,
    /// Nobody new; accounts that already linked it may still sign in.
    Nobody,
}

impl NewAccounts {
    #[must_use]
    pub fn as_i64(self) -> i64 {
        match self {
            Self::Anyone => 0,
            Self::Members => 1,
            Self::Nobody => 2,
        }
    }

    #[must_use]
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => Self::Members,
            2 => Self::Nobody,
            _ => Self::Anyone,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anyone => "anyone",
            Self::Members => "members",
            Self::Nobody => "none",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "anyone" => Self::Anyone,
            "members" => Self::Members,
            "none" => Self::Nobody,
            _ => return None,
        })
    }
}

/// A provider this server knows how to set up without being told its
/// addresses. Listed in Admin › Sign-in whether configured or not.
pub struct Preset {
    pub id: &'static str,
    pub kind: Kind,
    pub name: &'static str,
    pub issuer: Option<&'static str>,
    /// Where the operator registers this server as an app.
    pub setup_url: &'static str,
}

/// The built-in providers, in their default order: the groups people already
/// meet in first, then the free ones, then the big two.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "discord",
        kind: Kind::Discord,
        name: "Discord",
        issuer: None,
        setup_url: "https://discord.com/developers/applications",
    },
    Preset {
        id: "reddit",
        kind: Kind::Reddit,
        name: "Reddit",
        issuer: None,
        setup_url: "https://www.reddit.com/prefs/apps",
    },
    Preset {
        id: "mastodon",
        kind: Kind::Mastodon,
        name: "Mastodon",
        issuer: None,
        setup_url: "",
    },
    Preset {
        id: "codeberg",
        kind: Kind::Oidc,
        name: "Codeberg",
        issuer: Some("https://codeberg.org"),
        setup_url: "https://codeberg.org/user/settings/applications",
    },
    Preset {
        id: "google",
        kind: Kind::Oidc,
        name: "Google",
        issuer: Some("https://accounts.google.com"),
        setup_url: "https://console.cloud.google.com/apis/credentials",
    },
    Preset {
        id: "github",
        kind: Kind::Github,
        name: "GitHub",
        issuer: None,
        setup_url: "https://github.com/settings/developers",
    },
];

/// Where to register a provider as an app, for the setup form.
#[must_use]
pub fn setup_url(id: &str) -> &'static str {
    PRESETS
        .iter()
        .find(|p| p.id == id)
        .map_or("", |p| p.setup_url)
}

/// Why a round trip was started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// Somebody without a session: sign in, or make an account.
    SignIn,
    /// A signed-in account adds this provider to its ways in.
    Link(Uuid),
    /// A signed-in account proves it is still them (`auth::REAUTH_WINDOW`).
    Reauth(Uuid),
    /// The operator checks a provider's keys work.
    Test(Uuid),
    /// A moderator links a community to one of their groups there.
    Community { by: Uuid, community: Uuid },
}

/// A round trip in flight, between `start` and `callback`.
#[derive(Debug, Clone)]
pub struct Flow {
    pub provider: String,
    pub purpose: Purpose,
    /// PKCE: the provider is told its hash, and must be shown this.
    pub verifier: String,
    /// The fediverse server, for Mastodon.
    pub host: Option<String>,
    /// The app's pairing code, when the app started it.
    pub app: Option<String>,
    /// Where the web UI goes once done; always a path on this site.
    pub return_to: String,
    /// Whether to ask which groups the person is in (a community is linked to
    /// one at this provider) or which they run (`Purpose::Community`).
    pub want_groups: bool,
    pub created: Instant,
}

/// Who the person is at the provider, as far as this server needs to know.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    /// The provider's stable id for them.
    pub subject: String,
    /// What they are called there: `annadv`, `u/annadv`, `@anna@mastodon.nl`.
    pub handle: String,
    pub display_name: String,
    /// Only an address the provider says it verified.
    pub email: Option<String>,
    /// Groups they are in (Discord servers, subreddits), when asked.
    pub groups: Vec<Group>,
    /// Groups they run, when asked (`Purpose::Community`).
    pub managed: Vec<Group>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Group {
    pub group_id: String,
    pub name: String,
}

/// What a round trip came to, kept for the web UI to read.
#[derive(Debug, Clone)]
pub enum Outcome {
    SignedIn {
        principal: Uuid,
        username: String,
        /// Handed out once; `None` after the first read.
        token: Option<String>,
        offers: Vec<store::Offer>,
    },
    ChooseName {
        suggestion: String,
        offers: Vec<store::Offer>,
        new_accounts: NewAccounts,
    },
    LinkExisting {
        username: String,
    },
    Linked,
    Reauthed,
    Tested,
    CommunityGroups {
        community: Uuid,
        community_name: String,
    },
    Refused(String),
    /// Spent: an account was made or linked with it already.
    Spent,
}

/// A round trip's answer.
#[derive(Debug, Clone)]
pub struct Ticket {
    pub provider: String,
    pub provider_name: String,
    pub kind: Kind,
    pub profile: Profile,
    pub outcome: Outcome,
    pub purpose: Purpose,
    pub app: Option<String>,
    pub return_to: String,
    pub created: Instant,
}

/// The app's side of a sign-in it handed to the browser.
#[derive(Debug, Clone)]
pub struct Pairing {
    secret_hash: String,
    created: Instant,
    /// The session made for the app, and its account name, once the browser's
    /// round trip is done.
    done: Option<(String, String)>,
}

/// Everything a sign-in keeps between requests.
pub struct SignIn {
    pub key: keys::SealKey,
    pub client: fetch::Client,
    flows: Mutex<HashMap<String, Flow>>,
    tickets: Mutex<HashMap<String, Ticket>>,
    pairings: Mutex<HashMap<String, Pairing>>,
    /// OpenID providers' published addresses, by issuer, an hour at a time.
    discovery: Mutex<HashMap<String, (providers::Endpoints, Instant)>>,
    /// Round trips started per caller: each costs this server a request to
    /// somebody else, and the fediverse's first one registers an app there.
    pub starts: Throttle,
    /// Tests only: where the stand-ins for Discord, Reddit and GitHub are
    /// (`providers::fixed_endpoints`).
    pub test_base: Option<String>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl SignIn {
    #[must_use]
    pub fn new(key: keys::SealKey, allow_private: bool, test_base: Option<String>) -> Self {
        Self {
            test_base,
            key,
            client: fetch::Client::new(allow_private),
            flows: Mutex::default(),
            tickets: Mutex::default(),
            pairings: Mutex::default(),
            discovery: Mutex::default(),
            starts: Throttle::new(60, Duration::from_secs(3600)),
        }
    }

    /// Remember a round trip; answers its `state`.
    pub fn remember(&self, flow: Flow) -> String {
        let state = crate::auth::random_token();
        let mut map = locked(&self.flows);
        map.retain(|_, f| f.created.elapsed() < FLOW_TTL);
        map.insert(state.clone(), flow);
        state
    }

    /// The round trip `state` names, once: a callback replayed finds nothing.
    pub fn take_flow(&self, state: &str) -> Option<Flow> {
        locked(&self.flows)
            .remove(state)
            .filter(|f| f.created.elapsed() < FLOW_TTL)
    }

    /// Keep a round trip's answer; answers the ticket the web UI reads it by.
    pub fn file(&self, ticket: Ticket) -> String {
        let id = crate::auth::random_token();
        let mut map = locked(&self.tickets);
        map.retain(|_, t| t.created.elapsed() < FLOW_TTL);
        map.insert(id.clone(), ticket);
        id
    }

    pub fn ticket(&self, id: &str) -> Option<Ticket> {
        locked(&self.tickets)
            .get(id)
            .filter(|t| t.created.elapsed() < FLOW_TTL)
            .cloned()
    }

    /// Change a ticket in place, if it is still there.
    pub fn update_ticket(&self, id: &str, f: impl FnOnce(&mut Ticket)) {
        if let Some(t) = locked(&self.tickets).get_mut(id) {
            f(t);
        }
    }

    /// A new pairing code for the app: a code to show, and the secret only
    /// the app holds, which it polls with.
    pub fn pair(&self) -> (String, String) {
        let secret = crate::auth::random_token();
        let mut map = locked(&self.pairings);
        map.retain(|_, p| p.created.elapsed() < FLOW_TTL);
        let code = loop {
            let code = pairing_code();
            if !map.contains_key(&code) {
                break code;
            }
        };
        map.insert(
            code.clone(),
            Pairing {
                secret_hash: crate::auth::token_hash(&secret),
                created: Instant::now(),
                done: None,
            },
        );
        (code, secret)
    }

    /// Whether `code` is a live pairing still waiting.
    #[must_use]
    pub fn pairing_waiting(&self, code: &str) -> bool {
        locked(&self.pairings)
            .get(code)
            .is_some_and(|p| p.created.elapsed() < FLOW_TTL && p.done.is_none())
    }

    /// Leave the app's session for it to collect.
    pub fn complete_pairing(&self, code: &str, token: String, username: String) {
        if let Some(p) = locked(&self.pairings).get_mut(code) {
            p.done = Some((token, username));
        }
    }

    /// The app polling: `None` for an unknown or expired code or a wrong
    /// secret, `Some(None)` while waiting, and the session once, after which
    /// the code is gone.
    pub fn poll_pairing(&self, code: &str, secret: &str) -> Option<Option<(String, String)>> {
        let mut map = locked(&self.pairings);
        let p = map.get(code)?;
        if p.created.elapsed() >= FLOW_TTL || p.secret_hash != crate::auth::token_hash(secret) {
            return None;
        }
        if p.done.is_none() {
            return Some(None);
        }
        map.remove(code).map(|p| p.done)
    }

    /// An OpenID provider's addresses, from its published configuration.
    pub fn endpoints(&self, issuer: &str) -> anyhow::Result<providers::Endpoints> {
        if let Some((e, at)) = locked(&self.discovery).get(issuer) {
            if at.elapsed() < Duration::from_secs(3600) {
                return Ok(e.clone());
            }
        }
        let e = providers::discover(&self.client, issuer)?;
        locked(&self.discovery).insert(issuer.to_string(), (e.clone(), Instant::now()));
        Ok(e)
    }
}

/// `KQ7-4MD`: seven characters a person can compare at a glance, without the
/// ones that look alike (0/O, 1/I/L).
fn pairing_code() -> String {
    const ABC: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut bytes = [0u8; 7];
    let _ = getrandom::getrandom(&mut bytes);
    let mut out: String = bytes
        .iter()
        .map(|b| ABC[usize::from(*b) % ABC.len()] as char)
        .collect();
    out.insert(3, '-');
    out
}

/// The name to suggest for a new account: the provider's own handle or given
/// name, made into something this server takes (`http::check_username`), or
/// empty for the person to choose.
#[must_use]
pub fn suggest_name(profile: &Profile) -> String {
    let handle = profile.handle.as_str();
    let source = handle
        .strip_prefix("u/")
        .unwrap_or(handle)
        .trim_start_matches('@')
        .split('@')
        .next()
        .unwrap_or_default();
    let source = if source.is_empty() {
        profile.display_name.as_str()
    } else {
        source
    };
    let name: String = source
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(32)
        .collect();
    name.trim_matches(['.', '_', '-'].as_slice()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_suggested_name_is_one_lower_case_word() {
        let p = |handle: &str, name: &str| Profile {
            handle: handle.into(),
            display_name: name.into(),
            ..Profile::default()
        };
        assert_eq!(suggest_name(&p("annadv", "Anna")), "annadv");
        assert_eq!(suggest_name(&p("@Anna@mastodon.nl", "")), "anna");
        assert_eq!(suggest_name(&p("u/Anna_dV", "")), "anna_dv");
        assert_eq!(suggest_name(&p("ulrike", "")), "ulrike");
        assert_eq!(suggest_name(&p("", "Anna de Vries")), "annadevries");
        assert_eq!(suggest_name(&p("", "🙂")), "");
    }

    #[test]
    fn a_pairing_hands_its_session_over_once_and_only_with_its_secret() {
        let s = SignIn::new(keys::SealKey::ephemeral().unwrap(), false, None);
        let (code, secret) = s.pair();
        assert_eq!(code.len(), 8);
        assert_eq!(s.poll_pairing(&code, &secret), Some(None));
        assert_eq!(s.poll_pairing(&code, "wrong"), None);
        assert!(s.pairing_waiting(&code));
        s.complete_pairing(&code, "tok".into(), "anna".into());
        assert!(!s.pairing_waiting(&code));
        assert_eq!(s.poll_pairing(&code, "wrong"), None);
        assert_eq!(
            s.poll_pairing(&code, &secret),
            Some(Some(("tok".into(), "anna".into())))
        );
        assert_eq!(s.poll_pairing(&code, &secret), None);
    }

    #[test]
    fn a_flow_is_taken_once() {
        let s = SignIn::new(keys::SealKey::ephemeral().unwrap(), false, None);
        let state = s.remember(Flow {
            provider: "discord".into(),
            purpose: Purpose::SignIn,
            verifier: "v".into(),
            host: None,
            app: None,
            return_to: "/".into(),
            want_groups: false,
            created: Instant::now(),
        });
        assert!(s.take_flow(&state).is_some());
        assert!(s.take_flow(&state).is_none());
    }
}
