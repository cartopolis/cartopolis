//! Outbound email: one message type, one relay, and a mode for when there is no
//! relay at all.
//!
//! # Why a relay and not a mail server
//!
//! The only mail this server sends is a verification link, and the only thing
//! that matters about it is that it *arrives*. Delivering directly from a VPS
//! means SPF, DKIM, DMARC, a PTR record and a reputation nobody here is going to
//! maintain, and the failure mode is silent: the mail is accepted by the far side
//! and filed in a spam folder, so registration looks broken to the person and
//! healthy from here. So this speaks SMTP to somebody else's relay
//! ([`MailConfig::smtp_url`]) and has no opinion about which.
//!
//! # The unconfigured mode is not a no-op
//!
//! With no `smtp_url` the mailer **logs the message, link included**, at warn
//! level, and reports success. That is what `bun run dev` against a local
//! simulator uses, and it is what the test suite asserts against — a registration
//! flow that can only be exercised with a live relay is a flow nobody exercises.
//! It is loud on purpose: a deployment that forgot to configure the relay is
//! writing account-activation links into its own log, and that should be visible
//! from the first registration rather than from the first complaint.

use anyhow::Context;
use lettre::message::{header::ContentType, Mailbox, Message};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use std::sync::Once;

/// Where mail goes and what it says it is from.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct MailConfig {
    /// `smtps://user:pass@smtp.example.com:465` (implicit TLS),
    /// `smtp://user:pass@smtp.example.com:587?tls=required` (STARTTLS), or `None`
    /// for the logging mode above.
    ///
    /// A URL rather than five fields because every relay's documentation is
    /// already written as one, and because it keeps the password out of the rest
    /// of the config surface — one secret, one env var (`CARTO_SMTP_URL`).
    pub smtp_url: Option<String>,
    /// The envelope and header sender. Must be an address the relay is willing to
    /// send as; most refuse anything else, which is a good refusal.
    pub from: String,
    /// Display name beside it.
    pub from_name: String,
    /// The origin verification links are built against — the public URL of the
    /// web app, e.g. `https://cartopolis.org`.
    ///
    /// Not derived from the request's `Host` header, deliberately: that header is
    /// attacker-controlled, and a link built from it is a link an attacker chose
    /// the destination of, mailed to a real person by this server, over its
    /// reputation.
    pub public_base_url: String,
}

impl Default for MailConfig {
    fn default() -> Self {
        Self {
            smtp_url: None,
            from: "cartopolis@localhost".into(),
            from_name: "Cartopolis".into(),
            public_base_url: "http://localhost:3100".into(),
        }
    }
}

/// rustls 0.23 resolves a process-wide `CryptoProvider` and has no built-in
/// default; without one installed, the first TLS connection **panics**. The
/// client installs one for exactly this reason (see `crates/cartopolis`'s
/// dependency note) and the server needs the same, because "it compiles and then
/// dies when somebody registers" is the failure that note exists to prevent.
fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // Fails only if something else installed one first, which is the outcome
        // this wants anyway.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The two ways this server can be sending mail.
pub enum Mailer {
    Smtp {
        transport: Box<AsyncSmtpTransport<Tokio1Executor>>,
        from: Mailbox,
    },
    /// No relay configured: messages are logged instead. See the module note.
    Logging,
}

impl Mailer {
    /// Build the mailer described by the config. Fails on a malformed SMTP URL or
    /// `from` address rather than falling back to logging — a deployment that
    /// mistyped its relay must find out at startup, not by quietly publishing
    /// activation links to its journal for a week.
    ///
    /// **Call this on a tokio runtime.** lettre's connection pool spawns a task
    /// when the transport is built and another when it is dropped, and off a
    /// runtime both panic — the drop one during unwinding, which aborts the
    /// process rather than failing anything. `serve` is async, so this is only a
    /// trap for a test or a tool that builds one from `main`.
    pub fn from_config(config: &MailConfig) -> anyhow::Result<Self> {
        let from: Mailbox = format!("{} <{}>", config.from_name, config.from)
            .parse()
            .with_context(|| format!("mail `from` address {:?}", config.from))?;
        let Some(url) = config
            .smtp_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        else {
            return Ok(Self::Logging);
        };
        install_crypto_provider();
        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .context("parse smtp_url")?
            .build();
        Ok(Self::Smtp {
            transport: Box::new(transport),
            from,
        })
    }

    /// Build an SMTP mailer from parts, for a relay whose credentials are easier
    /// to hand over separately than to percent-encode into a URL.
    pub fn smtp(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        from: Mailbox,
    ) -> anyhow::Result<Self> {
        install_crypto_provider();
        let transport = AsyncSmtpTransport::<Tokio1Executor>::relay(host)
            .context("smtp relay")?
            .port(port)
            .credentials(Credentials::new(user.to_string(), password.to_string()))
            .build();
        Ok(Self::Smtp {
            transport: Box::new(transport),
            from,
        })
    }

    /// Whether mail actually leaves this process.
    #[must_use]
    pub fn delivers(&self) -> bool {
        matches!(self, Self::Smtp { .. })
    }

    /// Send one plain-text message.
    ///
    /// Plain text and not HTML: the only content is a sentence and a URL, every
    /// client renders it, and an HTML mail with a linked button is the shape spam
    /// filters are most suspicious of.
    pub async fn send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<()> {
        match self {
            Self::Logging => {
                tracing::warn!(
                    %to,
                    %subject,
                    "no smtp_url configured — mail not sent, printing it instead:\n{body}"
                );
                Ok(())
            }
            Self::Smtp { transport, from } => {
                let to: Mailbox = to.parse().with_context(|| format!("recipient {to:?}"))?;
                let message = Message::builder()
                    .from(from.clone())
                    .to(to)
                    .subject(subject)
                    .header(ContentType::TEXT_PLAIN)
                    .body(body.to_string())
                    .context("build message")?;
                transport.send(message).await.context("smtp send")?;
                Ok(())
            }
        }
    }
}

impl std::fmt::Debug for Mailer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The SMTP URL carries a password; `AsyncSmtpTransport` has no Debug
            // that would print it, and this must not grow one.
            Self::Smtp { from, .. } => write!(f, "Mailer::Smtp {{ from: {from} }}"),
            Self::Logging => write!(f, "Mailer::Logging"),
        }
    }
}

/// The verification mail, as subject + body.
///
/// One function so the wording is in one place and the tests can assert on the
/// link without reaching into an SMTP transcript.
#[must_use]
pub fn verification_message(username: &str, link: &str, valid_hours: u64) -> (String, String) {
    let subject = "Confirm your Cartopolis account".to_string();
    let body = format!(
        "Hello {username},\n\
         \n\
         Someone — we hope you — created a Cartopolis account with this address.\n\
         Open this link to confirm it and finish signing up:\n\
         \n\
         {link}\n\
         \n\
         The link works once and expires in {valid_hours} hours.\n\
         \n\
         If this wasn't you, ignore this mail. The account stays inactive and is\n\
         removed on its own; nobody can sign in with it in the meantime.\n"
    );
    (subject, body)
}

/// What is sent when an operator activates an account by hand.
///
/// The person registered, waited for a link that never came and may well have
/// given up, so this is the mail that tells them it worked after all — and says
/// nothing about a link, because there is none to click.
#[must_use]
pub fn activated_message(username: &str, sign_in_url: &str) -> (String, String) {
    let subject = "Your Cartopolis account is active".to_string();
    let body = format!(
        "Hello {username},\n\
         \n\
         Your Cartopolis account has been activated. You can sign in now with\n\
         the username {username} and the password you chose when you registered,\n\
         in the Cartopolis app or here: {sign_in_url}\n\
         \n\
         If you did not register an account, reply to this mail and we will\n\
         remove it.\n"
    );
    (subject, body)
}

/// What is sent when somebody registers with an address that already has an
/// account.
///
/// The API answers that request with the same "check your inbox" it gives a real
/// registration — telling the caller "that address is taken" turns the form into
/// a tool for testing whether a person has an account here. So the person who
/// actually owns the address is told instead, which is both the privacy-preserving
/// answer and the useful one: they are the one who forgot.
#[must_use]
pub fn already_registered_message(username: &str, sign_in_url: &str) -> (String, String) {
    let subject = "You already have a Cartopolis account".to_string();
    let body = format!(
        "Hello,\n\
         \n\
         Someone tried to register a Cartopolis account with this address, but it\n\
         already has one: {username}.\n\
         \n\
         Sign in here instead: {sign_in_url}\n\
         \n\
         No new account was created and nothing has changed. If this wasn't you,\n\
         there is nothing you need to do.\n"
    );
    (subject, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_relay_is_the_logging_mailer() {
        let mailer = Mailer::from_config(&MailConfig::default()).unwrap();
        assert!(!mailer.delivers());
    }

    #[test]
    fn a_blank_relay_is_the_logging_mailer() {
        // An env var set to the empty string is how a deployment "unsets" one,
        // and figment hands it through as `Some("")`.
        let config = MailConfig {
            smtp_url: Some("   ".into()),
            ..MailConfig::default()
        };
        assert!(!Mailer::from_config(&config).unwrap().delivers());
    }

    #[test]
    fn a_mistyped_relay_is_an_error_not_a_fallback() {
        let config = MailConfig {
            smtp_url: Some("http://not-smtp.example.com".into()),
            ..MailConfig::default()
        };
        assert!(Mailer::from_config(&config).is_err());
    }

    #[test]
    fn a_mistyped_sender_is_an_error() {
        let config = MailConfig {
            from: "not an address".into(),
            ..MailConfig::default()
        };
        assert!(Mailer::from_config(&config).is_err());
    }

    #[tokio::test]
    async fn the_logging_mailer_reports_success() {
        let mailer = Mailer::from_config(&MailConfig::default()).unwrap();
        mailer
            .send("someone@example.com", "hi", "body")
            .await
            .unwrap();
    }

    #[test]
    fn the_activation_mail_names_the_account() {
        let (subject, body) = activated_message("ada", "https://example.com");
        assert!(subject.contains("active"));
        assert!(body.contains("ada"));
        assert!(body.contains("https://example.com"));
    }

    #[test]
    fn the_verification_mail_carries_the_link() {
        let (subject, body) =
            verification_message("ada", "https://example.com/verify?token=abc", 24);
        assert!(subject.contains("Cartopolis"));
        assert!(body.contains("https://example.com/verify?token=abc"));
        assert!(body.contains("ada"));
        assert!(body.contains("24 hours"));
    }

    // On a runtime, because building (and dropping) a pooled transport needs
    // one — see `from_config`.
    #[tokio::test]
    async fn the_debug_impl_cannot_print_a_password() {
        let config = MailConfig {
            smtp_url: Some("smtps://someone:hunter2@smtp.example.com:465".into()),
            ..MailConfig::default()
        };
        let printed = format!("{:?}", Mailer::from_config(&config).unwrap());
        assert!(!printed.contains("hunter2"));
    }
}
