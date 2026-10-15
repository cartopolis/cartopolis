//! Resolve connect credentials for the ADR-016 handshake, so you don't log in each
//! run. Precedence: explicit `--login` (a file, or inline `user:pass`) → a default
//! login file → `CARTO_TOKEN` env → `CARTO_USER`/`CARTO_PASSWORD` env → the
//! `dev:dev` default a freshly provisioned dev sim answers to.
//!
//! `CARTO_TOKEN` is an **access token** made on the server's Tokens page
//! (`docs/notes/access-tokens.md`): how a bot signs in without its password ever
//! being on the machine it runs on. It is not remembered in `user.json` — the
//! environment already has it, and a second copy on disk would outlive the
//! process that was given it.
//!
//! Login file (gitignored TOML), all fields optional:
//! ```toml
//! server   = "host:port"   # used when --connect is absent
//! user     = "alice"
//! password = "secret"
//! # or, instead of user/password:
//! token    = "…"           # a session token from a previous run
//! ```

use std::path::Path;

use cartopolis_core::AuthCredential;
use serde::Deserialize;

/// Default login file checked when `--login` is omitted. Gitignored.
const DEFAULT_LOGIN_FILE: &str = "data/dev-login.toml";
const DEV_USER: &str = "dev";
const DEV_PASSWORD: &str = "dev";

#[derive(Deserialize, Default)]
struct LoginFile {
    server: Option<String>,
    user: Option<String>,
    password: Option<String>,
    token: Option<String>,
}

/// Resolved login: a credential, plus an optional server the file pinned.
pub struct Login {
    /// Server address from the login file, if any. Used only when `--connect`
    /// is absent (lets one file pin both destination and identity).
    pub server: Option<String>,
    pub credential: AuthCredential,
    /// Whether the session token the server answers with should be remembered
    /// in `user.json` for a silent reconnect. Not for `CARTO_TOKEN`.
    pub persist_token: bool,
}

/// Resolve credentials from the optional `--login` value.
pub fn resolve(cli_login: Option<&str>) -> Login {
    if let Some(value) = cli_login {
        // An existing file wins; otherwise treat the value as inline `user:pass`.
        if Path::new(value).is_file() {
            return from_file(value).unwrap_or_else(|e| {
                tracing::error!("--login file {value}: {e:#}; falling back to dev account");
                dev_login()
            });
        }
        if let Some(credential) = parse_inline(value) {
            return Login {
                server: None,
                credential,
                persist_token: true,
            };
        }
        tracing::error!("--login {value} is neither a file nor user:pass; using dev account");
        return dev_login();
    }
    if Path::new(DEFAULT_LOGIN_FILE).is_file() {
        match from_file(DEFAULT_LOGIN_FILE) {
            Ok(login) => return login,
            Err(e) => tracing::error!("{DEFAULT_LOGIN_FILE}: {e:#}; using env/dev"),
        }
    }
    env_or_dev()
}

fn parse_inline(value: &str) -> Option<AuthCredential> {
    let (user, pass) = value.split_once(':')?;
    (!user.is_empty()).then(|| AuthCredential::Password {
        username: user.to_string(),
        password: pass.to_string(),
    })
}

fn from_file(path: &str) -> anyhow::Result<Login> {
    let file: LoginFile = toml::from_str(&std::fs::read_to_string(path)?)?;
    let credential = match file.token {
        Some(token) => AuthCredential::Token(token),
        None => AuthCredential::Password {
            username: file.user.unwrap_or_else(|| DEV_USER.to_string()),
            password: file.password.unwrap_or_else(|| DEV_PASSWORD.to_string()),
        },
    };
    Ok(Login {
        server: file.server,
        credential,
        persist_token: true,
    })
}

fn env_or_dev() -> Login {
    from_env(
        std::env::var("CARTO_TOKEN").ok(),
        std::env::var("CARTO_USER").ok(),
        std::env::var("CARTO_PASSWORD").ok(),
    )
}

/// [`env_or_dev`] with the environment passed in, so the precedence is testable
/// without mutating the process environment under a parallel test runner.
fn from_env(token: Option<String>, user: Option<String>, password: Option<String>) -> Login {
    if let Some(token) = token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
    {
        return Login {
            server: None,
            credential: AuthCredential::Token(token),
            persist_token: false,
        };
    }
    Login {
        server: None,
        credential: AuthCredential::Password {
            username: user.unwrap_or_else(|| DEV_USER.to_string()),
            password: password.unwrap_or_else(|| DEV_PASSWORD.to_string()),
        },
        persist_token: true,
    }
}

fn dev_login() -> Login {
    Login {
        server: None,
        credential: AuthCredential::Password {
            username: DEV_USER.to_string(),
            password: DEV_PASSWORD.to_string(),
        },
        persist_token: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An access token in the environment wins over a username and password
    /// there, and is not remembered; a blank one is no token at all.
    #[test]
    fn an_env_token_wins_and_is_not_persisted() {
        let login = from_env(Some(" abc ".into()), Some("u".into()), Some("p".into()));
        assert!(matches!(login.credential, AuthCredential::Token(ref t) if t == "abc"));
        assert!(!login.persist_token);

        let login = from_env(Some("  ".into()), Some("u".into()), Some("p".into()));
        assert!(matches!(
            login.credential,
            AuthCredential::Password { ref username, ref password } if username == "u" && password == "p"
        ));
        assert!(login.persist_token);
    }
}
