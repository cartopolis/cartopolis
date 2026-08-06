//! Resolve connect credentials for the ADR-016 handshake, so you don't log in each
//! run. Precedence: explicit `--login` (a file, or inline `user:pass`) → a default
//! login file → `VIBE_USER`/`VIBE_PASSWORD` env → the solo `dev` account.
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

use serde::Deserialize;
use viberfox_core::AuthCredential;

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
    })
}

fn env_or_dev() -> Login {
    Login {
        server: None,
        credential: AuthCredential::Password {
            username: std::env::var("VIBE_USER").unwrap_or_else(|_| DEV_USER.to_string()),
            password: std::env::var("VIBE_PASSWORD").unwrap_or_else(|_| DEV_PASSWORD.to_string()),
        },
    }
}

fn dev_login() -> Login {
    Login {
        server: None,
        credential: AuthCredential::Password {
            username: DEV_USER.to_string(),
            password: DEV_PASSWORD.to_string(),
        },
    }
}
