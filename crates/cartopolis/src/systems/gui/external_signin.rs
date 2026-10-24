//! Signing in with another account from the app: Discord, Google and the rest
//! (`docs/notes/external-sign-in.md`, the server's `signin` module).
//!
//! **The browser does the signing in, and the app collects the session.** The
//! providers will not show their sign-in inside an embedded view, so the app
//! asks the server for a pairing code, opens the system browser at the
//! provider's button with that code, and polls until the browser's round trip
//! leaves a session there. A redirect back into the app would be simpler, but
//! an Android app link does not reach an app that is already running
//! (`platform::android::launch_url`), and this one always is.
//!
//! The code is on the card and on the browser's last page, so the person can
//! see the browser is finishing *this* sign-in.

use bevy::tasks::{IoTaskPool, Task};

use crate::platform::http::{self, Request};

/// Seconds between polls while the browser is out.
const POLL_S: f64 = 2.0;

/// One provider the server offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offered {
    pub id: String,
    pub name: String,
}

/// A sign-in handed to the browser.
#[derive(Debug, Clone)]
pub struct Pending {
    pub base: String,
    pub provider: Offered,
    pub code: String,
    secret: String,
    next_poll: f64,
}

/// What a finished pairing gives the sign-in card.
pub struct Collected {
    pub token: String,
    pub username: String,
}

/// The card's other way in: what the server offers and a round trip in flight.
#[derive(Default)]
pub struct External {
    /// The API base the offer list is for; another server means another list.
    listed_for: Option<String>,
    pub offered: Vec<Offered>,
    listing: Option<(String, Task<Result<String, String>>)>,
    pairing: Option<(String, Offered, Task<Result<String, String>>)>,
    polling: Option<Task<Result<String, String>>>,
    pub pending: Option<Pending>,
    pub error: Option<String>,
}

/// Whether this build can hand a sign-in to a browser at all. The web build
/// has no `open_url`, and runs on cartopolis.org, whose own page has the buttons.
#[must_use]
pub fn supported() -> bool {
    crate::systems::data_layers::open_url_supported()
}

fn offered_from(json: &str) -> Vec<Offered> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    v["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            Some(Offered {
                id: p["id"].as_str()?.to_string(),
                name: p["name"].as_str()?.to_string(),
            })
        })
        .collect()
}

impl External {
    /// Keep the offer list in step with the server field: ask the server at
    /// `base` once, and again when it changes. `None` (no field, a dev sim on
    /// this machine) offers nothing.
    pub fn follow(&mut self, base: Option<String>) {
        if let Some((for_base, task)) = &mut self.listing {
            if let Some(result) = http::poll_task(task) {
                let for_base = for_base.clone();
                self.listing = None;
                self.offered = result.map(|s| offered_from(&s)).unwrap_or_default();
                self.listed_for = Some(for_base);
            } else {
                return;
            }
        }
        if base == self.listed_for {
            return;
        }
        let Some(base) = base.filter(|_| supported()) else {
            self.offered.clear();
            self.listed_for = None;
            return;
        };
        let url = format!("{base}/api/register");
        let task = IoTaskPool::get().spawn(async move {
            http::fetch_string(Request::get(url).header("Accept", "application/json")).await
        });
        self.listing = Some((base, task));
    }

    /// Start a sign-in with `provider`: get a pairing code, then open the
    /// browser (in [`Self::tick`], once the code is here).
    pub fn start(&mut self, base: &str, provider: Offered) {
        self.error = None;
        self.pending = None;
        let url = format!("{base}/api/auth/app");
        let task = IoTaskPool::get().spawn(async move {
            http::fetch_string(
                Request::post(url)
                    .header("Content-Type", "application/json")
                    .body(b"{}".to_vec()),
            )
            .await
        });
        self.pairing = Some((base.to_string(), provider, task));
    }

    /// Open the browser on the round trip again (it was closed by mistake).
    pub fn reopen(&self) {
        if let Some(p) = &self.pending {
            open(p);
        }
    }

    pub fn cancel(&mut self) {
        self.pending = None;
        self.pairing = None;
        self.polling = None;
    }

    #[must_use]
    pub fn busy(&self) -> bool {
        self.pairing.is_some() || self.pending.is_some()
    }

    /// Advance whatever is in flight; the session, the frame the browser's
    /// round trip leaves one for the app.
    pub fn tick(&mut self, now: f64) -> Option<Collected> {
        if let Some((base, provider, task)) = &mut self.pairing {
            if let Some(result) = http::poll_task(task) {
                let (base, provider) = (base.clone(), provider.clone());
                self.pairing = None;
                let parsed = result
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
                match parsed.as_ref().and_then(|v| {
                    Some((
                        v["code"].as_str()?.to_string(),
                        v["secret"].as_str()?.to_string(),
                    ))
                }) {
                    Some((code, secret)) => {
                        let p = Pending {
                            base,
                            provider,
                            code,
                            secret,
                            next_poll: now + POLL_S,
                        };
                        open(&p);
                        self.pending = Some(p);
                    }
                    None => self.error = Some("The server didn't answer. Try again.".into()),
                }
            }
        }
        if let Some(task) = &mut self.polling {
            let result = http::poll_task(task)?;
            self.polling = None;
            match result {
                Ok(body) => {
                    let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    if v["status"] == "done" {
                        self.pending = None;
                        return Some(Collected {
                            token: v["token"].as_str().unwrap_or_default().to_string(),
                            username: v["username"].as_str().unwrap_or_default().to_string(),
                        });
                    }
                }
                Err(e) if http::is_absent(&e) => {
                    self.pending = None;
                    self.error = Some("That sign-in expired. Start it again.".into());
                }
                // A dropped connection: the next poll tries again.
                Err(_) => {}
            }
        }
        let p = self.pending.as_mut()?;
        if now >= p.next_poll {
            p.next_poll = now + POLL_S;
            let url = format!("{}/api/auth/app/{}?secret={}", p.base, p.code, p.secret);
            self.polling = Some(IoTaskPool::get().spawn(async move {
                http::fetch_string(Request::get(url).header("Accept", "application/json")).await
            }));
        }
        None
    }
}

fn open(p: &Pending) {
    crate::systems::data_layers::open_url(&format!(
        "{}/api/auth/{}/start?app={}&return=/",
        p.base, p.provider.id, p.code
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offer_list_is_read_from_the_registration_rules() {
        let json = r#"{"open":true,"providers":[{"id":"discord","kind":"discord","name":"Discord"},{"id":"x"}]}"#;
        assert_eq!(
            offered_from(json),
            vec![Offered {
                id: "discord".into(),
                name: "Discord".into()
            }]
        );
        // A server older than sign-in with other accounts says nothing about it.
        assert!(offered_from(r#"{"open":true}"#).is_empty());
        assert!(offered_from("not json").is_empty());
    }
}
