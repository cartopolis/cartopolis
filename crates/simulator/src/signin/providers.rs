//! What each provider is asked, and how its answers are read. Blocking (the
//! guarded `fetch::Client`), so every call comes from `spawn_blocking`.
//!
//! **No ID token is checked; the profile is read with the access token.**
//! OpenID Connect allows that for a confidential client that received the
//! token straight from the provider's token endpoint over TLS (Core 1.0,
//! §3.1.3.7): the token cannot have been swapped on the way. It saves a JWT
//! library and key-set caching, and it is the one path Discord, Reddit, GitHub
//! and Mastodon offer anyway, none of which issue ID tokens.

use base64::Engine as _;
use serde_json::Value;
use sha2::Digest as _;

use super::{Group, Kind, Profile};
use crate::fediverse::fetch::{Client, FetchError};

/// Where a provider's three steps are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub authorize: String,
    pub token: String,
    pub userinfo: String,
}

/// Discord's permission bit for managing a server: what it takes to link one
/// to a community.
const MANAGE_GUILD: u64 = 0x20;

/// The addresses of the providers that are not discovered, rooted at
/// `test_base` in the integration tests (`SimConfig::signin_test_base`).
#[must_use]
pub fn fixed_endpoints(kind: Kind, test_base: Option<&str>) -> Option<Endpoints> {
    let at = |real: &str, name: &str, path: &str| match test_base {
        Some(base) => format!("{base}/{name}{path}"),
        None => format!("{real}{path}"),
    };
    Some(match kind {
        Kind::Discord => Endpoints {
            authorize: at("https://discord.com", "discord", "/oauth2/authorize"),
            token: at("https://discord.com", "discord", "/api/oauth2/token"),
            userinfo: at("https://discord.com", "discord", "/api/users/@me"),
        },
        Kind::Reddit => Endpoints {
            authorize: at("https://www.reddit.com", "reddit", "/api/v1/authorize"),
            token: at("https://www.reddit.com", "reddit", "/api/v1/access_token"),
            userinfo: at("https://oauth.reddit.com", "reddit", "/api/v1/me"),
        },
        Kind::Github => Endpoints {
            authorize: at("https://github.com", "github", "/login/oauth/authorize"),
            token: at("https://github.com", "github", "/login/oauth/access_token"),
            userinfo: at("https://api.github.com", "github", "/user"),
        },
        Kind::Mastodon | Kind::Oidc => return None,
    })
}

/// A Mastodon-compatible server's addresses.
#[must_use]
pub fn mastodon_endpoints(origin: &str) -> Endpoints {
    Endpoints {
        authorize: format!("{origin}/oauth/authorize"),
        token: format!("{origin}/oauth/token"),
        userinfo: format!("{origin}/api/v1/accounts/verify_credentials"),
    }
}

/// An OpenID provider's addresses, from `{issuer}/.well-known/openid-configuration`.
pub fn discover(client: &Client, issuer: &str) -> anyhow::Result<Endpoints> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let doc = client.call_json("GET", &url, &[], None)?;
    let field = |k: &str| {
        doc[k]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("{issuer} publishes no {k}"))
    };
    Ok(Endpoints {
        authorize: field("authorization_endpoint")?,
        token: field("token_endpoint")?,
        userinfo: field("userinfo_endpoint")?,
    })
}

/// What to ask for: as little as the purpose needs.
#[must_use]
pub fn scopes(kind: Kind, groups: bool, managed: bool) -> &'static str {
    match (kind, groups || managed) {
        (Kind::Discord, false) => "identify email",
        (Kind::Discord, true) if managed => "identify guilds",
        (Kind::Discord, true) => "identify email guilds",
        (Kind::Reddit, false) => "identity",
        (Kind::Reddit, true) => "identity mysubreddits",
        (Kind::Github, _) => "read:user user:email",
        (Kind::Mastodon, _) => "read:accounts",
        (Kind::Oidc, _) => "openid email profile",
    }
}

/// PKCE's challenge for a verifier (RFC 7636, S256).
#[must_use]
pub fn challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier))
}

/// The address the browser is sent to.
#[must_use]
pub fn authorize_url(
    kind: Kind,
    endpoints: &Endpoints,
    client_id: &str,
    redirect_uri: &str,
    scope: &str,
    state: &str,
    verifier: &str,
) -> String {
    let mut url = url::Url::parse(&endpoints.authorize)
        .unwrap_or_else(|_| url::Url::parse("https://invalid.example/").expect("static URL"));
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", scope)
            .append_pair("state", state)
            .append_pair("code_challenge", &challenge(verifier))
            .append_pair("code_challenge_method", "S256");
        match kind {
            // Reddit refuses a request without it.
            Kind::Reddit => {
                q.append_pair("duration", "temporary");
            }
            // Otherwise a browser already signed in to one Google account
            // goes straight through, and somebody with two never gets to
            // choose.
            Kind::Oidc => {
                q.append_pair("prompt", "select_account");
            }
            _ => {}
        }
    }
    url.into()
}

/// The keys a round trip spends.
pub struct Credentials<'a> {
    pub client_id: &'a str,
    pub client_secret: &'a str,
}

/// Swap the code for an access token.
pub fn exchange(
    client: &Client,
    kind: Kind,
    endpoints: &Endpoints,
    keys: &Credentials<'_>,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> anyhow::Result<String> {
    let basic;
    let mut headers: Vec<(&str, &str)> = Vec::new();
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ];
    if kind == Kind::Reddit {
        // Reddit takes the client's keys only as Basic authentication.
        basic = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", keys.client_id, keys.client_secret))
        );
        headers.push(("Authorization", &basic));
    } else {
        form.push(("client_id", keys.client_id));
        form.push(("client_secret", keys.client_secret));
    }
    let answer = client
        .call_json("POST", &endpoints.token, &headers, Some(&form))
        .map_err(|e| anyhow::anyhow!("the provider did not accept the sign-in: {e}"))?;
    answer["access_token"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            // GitHub answers 200 with an error body.
            let why = answer["error_description"]
                .as_str()
                .or(answer["error"].as_str())
                .unwrap_or("no access token");
            anyhow::anyhow!("the provider did not accept the sign-in: {why}")
        })
}

fn get(client: &Client, url: &str, token: &str) -> Result<Value, FetchError> {
    let bearer = format!("Bearer {token}");
    client.call_json("GET", url, &[("Authorization", &bearer)], None)
}

fn text(v: &Value, k: &str) -> String {
    match &v[k] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Who signed in, and — when asked — which groups they are in or run.
///
/// `origin` is the base the fixed endpoints hang off, for the group lists that
/// live beside the profile (`fixed_endpoints`); `host` is the fediverse
/// server's name.
pub fn profile(
    client: &Client,
    kind: Kind,
    endpoints: &Endpoints,
    token: &str,
    host: Option<&str>,
    groups: bool,
    managed: bool,
) -> anyhow::Result<Profile> {
    let me = get(client, &endpoints.userinfo, token)?;
    let mut p = Profile::default();
    match kind {
        Kind::Oidc => {
            p.subject = text(&me, "sub");
            p.handle = [&me["preferred_username"], &me["email"], &me["name"]]
                .iter()
                .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
                .unwrap_or_default()
                .to_string();
            p.display_name = text(&me, "name");
            // Some providers send "true" as a string.
            let verified = me["email_verified"].as_bool() == Some(true)
                || me["email_verified"].as_str() == Some("true");
            p.email = verified
                .then(|| text(&me, "email"))
                .filter(|e| e.contains('@'));
        }
        Kind::Github => {
            p.subject = text(&me, "id");
            p.handle = text(&me, "login");
            p.display_name = text(&me, "name");
            let emails_url = endpoints.userinfo.clone() + "/emails";
            if let Ok(Value::Array(list)) = get(client, &emails_url, token) {
                p.email = list
                    .iter()
                    .find(|e| e["primary"] == true && e["verified"] == true)
                    .map(|e| text(e, "email"));
            }
        }
        Kind::Discord => {
            p.subject = text(&me, "id");
            p.handle = text(&me, "username");
            p.display_name = me["global_name"]
                .as_str()
                .map_or_else(|| p.handle.clone(), str::to_string);
            p.email = (me["verified"] == true)
                .then(|| text(&me, "email"))
                .filter(|e| e.contains('@'));
            if groups || managed {
                let url = endpoints.userinfo.clone() + "/guilds";
                if let Value::Array(list) = get(client, &url, token)? {
                    for g in &list {
                        let group = Group {
                            group_id: text(g, "id"),
                            name: text(g, "name"),
                        };
                        let perms: u64 = text(g, "permissions").parse().unwrap_or(0);
                        if g["owner"] == true || perms & MANAGE_GUILD != 0 {
                            p.managed.push(group.clone());
                        }
                        p.groups.push(group);
                    }
                }
            }
        }
        Kind::Reddit => {
            p.subject = text(&me, "id");
            p.handle = format!("u/{}", text(&me, "name"));
            p.display_name = text(&me, "name");
            let base = endpoints
                .userinfo
                .trim_end_matches("/api/v1/me")
                .to_string();
            let list = |which: &str| -> anyhow::Result<Vec<Group>> {
                let mut out = Vec::new();
                let mut after = String::new();
                // A few pages: enough for anybody's subscriptions, and a bound
                // on what one sign-in costs.
                for _ in 0..5 {
                    let url = format!("{base}/subreddits/mine/{which}?limit=100&after={after}");
                    let page = get(client, &url, token)?;
                    for child in page["data"]["children"].as_array().into_iter().flatten() {
                        let name = text(&child["data"], "display_name");
                        if !name.is_empty() {
                            out.push(Group {
                                group_id: name.to_lowercase(),
                                name: format!("r/{name}"),
                            });
                        }
                    }
                    after = text(&page["data"], "after");
                    if after.is_empty() {
                        break;
                    }
                }
                Ok(out)
            };
            if groups {
                p.groups = list("subscriber")?;
            }
            if managed {
                p.managed = list("moderator")?;
            }
        }
        Kind::Mastodon => {
            let host = host.unwrap_or_default();
            p.subject = format!("{}@{host}", text(&me, "id"));
            p.handle = format!("@{}@{host}", text(&me, "username"));
            p.display_name = me["display_name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map_or_else(|| text(&me, "username"), str::to_string);
        }
    }
    if p.subject.is_empty() {
        anyhow::bail!("the provider did not say who signed in");
    }
    Ok(p)
}

/// Register this server as an app with a fediverse server, the first time
/// somebody from there signs in: its client id and secret.
pub fn register_mastodon_app(
    client: &Client,
    origin: &str,
    redirect_uri: &str,
    website: &str,
) -> anyhow::Result<(String, String)> {
    let answer = client.call_json(
        "POST",
        &format!("{origin}/api/v1/apps"),
        &[],
        Some(&[
            ("client_name", "Cartopolis"),
            ("redirect_uris", redirect_uri),
            ("scopes", scopes(Kind::Mastodon, false, false)),
            ("website", website),
        ]),
    )?;
    let id = text(&answer, "client_id");
    let secret = text(&answer, "client_secret");
    if id.is_empty() || secret.is_empty() {
        anyhow::bail!("{origin} did not register this server as an app");
    }
    Ok((id, secret))
}

/// The server part of what somebody typed as their fediverse address: `anna@mastodon.nl`,
/// `@anna@mastodon.nl`, `https://mastodon.nl/@anna` or `mastodon.nl` all give
/// `mastodon.nl`. `None` for something that is not a host name.
#[must_use]
pub fn fediverse_host(typed: &str) -> Option<String> {
    let s = typed.trim();
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let s = s.split('/').next().unwrap_or_default();
    let host = s.rsplit('@').next().unwrap_or_default().to_lowercase();
    let ok = host.contains('.')
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-']);
    ok.then_some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_way_of_typing_a_fediverse_address_finds_its_server() {
        for typed in [
            "anna@mastodon.nl",
            "@anna@mastodon.nl",
            " Mastodon.NL ",
            "https://mastodon.nl/@anna",
        ] {
            assert_eq!(fediverse_host(typed).as_deref(), Some("mastodon.nl"), "{typed}");
        }
        assert_eq!(fediverse_host("anna"), None);
        assert_eq!(fediverse_host("evil.example/../x"), Some("evil.example".into()));
        assert_eq!(fediverse_host("a b.nl"), None);
    }

    #[test]
    fn the_pkce_challenge_is_rfc_7636s() {
        // The worked example in RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_reddit_authorize_address_asks_for_a_temporary_grant() {
        let e = fixed_endpoints(Kind::Reddit, None).unwrap();
        let url = authorize_url(
            Kind::Reddit,
            &e,
            "id",
            "https://cartopolis.org/api/auth/reddit/callback",
            "identity",
            "st",
            "v",
        );
        assert!(url.starts_with("https://www.reddit.com/api/v1/authorize?"));
        assert!(url.contains("duration=temporary"));
        assert!(url.contains("state=st"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fcartopolis.org%2Fapi%2Fauth%2Freddit%2Fcallback"));
    }
}
