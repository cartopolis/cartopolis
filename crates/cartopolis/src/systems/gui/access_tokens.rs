//! Settings ▸ Access tokens: what a bot or a script signs in with instead of
//! your password (`docs/notes/access-tokens.md`,
//! `docs/mockups/home-and-profile/v1`, F and G).
//!
//! The web page has had this since the tokens landed; this is the same three
//! things in the app — the list, the form, and the secret shown once — over the
//! same routes (`/api/tokens`). Two rules are the server's and are only
//! followed here: making a token asks for the password again, because a phone
//! left signed in should not be enough to mint a key that outlives it; and the
//! secret is in the answer to that one request and nowhere else, so it is held
//! in memory until Done and then dropped.

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use serde::Deserialize;
use std::sync::{Arc, Mutex};

use crate::systems::{icons, theme};

/// One token, without its secret (`auth::AccessTokenInfo` on the server).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TokenInfo {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub elevated: bool,
    /// Unix seconds.
    pub created_at: u64,
    #[serde(default)]
    pub last_used: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<u64>,
}

#[derive(Deserialize)]
struct ListDoc {
    tokens: Vec<TokenInfo>,
}

#[derive(Deserialize)]
struct MadeDoc {
    token: String,
    info: TokenInfo,
}

/// How long a new token lasts, as the web page offers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Expiry {
    Day,
    Month,
    #[default]
    Never,
}

impl Expiry {
    fn seconds(self) -> Option<u64> {
        match self {
            Expiry::Day => Some(24 * 3600),
            Expiry::Month => Some(30 * 24 * 3600),
            Expiry::Never => None,
        }
    }
}

enum Reply {
    List(Result<Vec<TokenInfo>, String>),
    Made(Result<(String, TokenInfo), String>),
    Revoked(Result<String, String>),
}

#[derive(Resource, Default)]
pub struct AccessTokens {
    /// `None` until asked; then the list or why it could not be had.
    pub list: Option<Result<Vec<TokenInfo>, String>>,
    pub label: String,
    pub expiry: Expiry,
    pub elevated: bool,
    /// The password, typed to confirm. Cleared the moment the request goes.
    pub password: String,
    /// The token just made: its name and its secret, shown until Done.
    pub shown: Option<(String, String)>,
    pub busy: bool,
    pub error: Option<String>,
    /// The list has been asked for since the page opened.
    asked: bool,
    inbox: Arc<Mutex<Vec<Reply>>>,
    /// A scripted capture's stand-in list (`--ui tokens`).
    pub demo: bool,
}

fn now_s() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl AccessTokens {
    fn spawn(&self, work: impl bevy::tasks::ConditionalSendFuture<Output = Reply> + 'static) {
        let inbox = self.inbox.clone();
        IoTaskPool::get()
            .spawn(async move {
                let reply = work.await;
                if let Ok(mut v) = inbox.lock() {
                    v.push(reply);
                }
            })
            .detach();
    }

    /// Ask for the list. Called when the page opens.
    pub fn refresh(&mut self) {
        if self.demo {
            return;
        }
        self.spawn(async {
            let got = crate::systems::model_assets::api_get("/api/tokens")
                .await
                .and_then(|b| {
                    serde_json::from_str::<ListDoc>(&b)
                        .map(|d| d.tokens)
                        .map_err(|e| format!("unexpected answer: {e}"))
                });
            Reply::List(got)
        });
    }

    fn make(&mut self) {
        let label = self.label.trim().to_string();
        let body = serde_json::json!({
            "label": label,
            "password": std::mem::take(&mut self.password),
            "elevated": self.elevated,
            "expires_at": self.expiry.seconds().map(|s| now_s() + s),
        })
        .to_string();
        self.busy = true;
        self.error = None;
        if self.demo {
            let info = TokenInfo {
                id: format!("demo{}", now_s()),
                label: label.clone(),
                created_at: now_s(),
                expires_at: self.expiry.seconds().map(|s| now_s() + s),
                ..default()
            };
            if let Ok(mut v) = self.inbox.lock() {
                v.push(Reply::Made(Ok((
                    "cpt_7Qe2mZx9Lr4vHc1sKd8aWf3nUj6yBt0pGo5iXe".into(),
                    info,
                ))));
            }
            return;
        }
        self.spawn(async move {
            let got = crate::systems::model_assets::api_post("/api/tokens", Some(body))
                .await
                .and_then(|b| {
                    serde_json::from_str::<MadeDoc>(&b)
                        .map(|d| (d.token, d.info))
                        .map_err(|e| format!("unexpected answer: {e}"))
                });
            Reply::Made(got)
        });
    }

    fn revoke(&mut self, token: &TokenInfo) {
        let id = token.id.clone();
        let label = token.label.clone();
        if self.demo {
            if let Some(Ok(list)) = &mut self.list {
                list.retain(|t| t.id != id);
            }
            return;
        }
        self.spawn(async move {
            let got = crate::systems::model_assets::api_delete(&format!("/api/tokens/{id}"))
                .await
                .map(|()| label);
            Reply::Revoked(got)
        });
    }

    fn drain(&mut self) {
        let replies = self
            .inbox
            .lock()
            .map(|mut v| std::mem::take(&mut *v))
            .unwrap_or_default();
        for reply in replies {
            match reply {
                Reply::List(r) => self.list = Some(r),
                Reply::Made(Ok((secret, info))) => {
                    self.busy = false;
                    self.shown = Some((info.label.clone(), secret));
                    self.label.clear();
                    if let Some(Ok(list)) = &mut self.list {
                        list.insert(0, info);
                    } else {
                        self.refresh();
                    }
                }
                Reply::Made(Err(e)) => {
                    self.busy = false;
                    self.error = Some(e);
                }
                Reply::Revoked(Ok(_)) => self.refresh(),
                Reply::Revoked(Err(e)) => self.error = Some(e),
            }
        }
    }

    /// Forget the secret and the password: when the page closes.
    pub fn forget(&mut self) {
        self.shown = None;
        self.password.clear();
        self.asked = false;
    }
}

/// `1760600000` as `16 Oct`.
fn day(at: u64) -> String {
    jiff::Timestamp::from_second(at as i64)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%-d %b")
                .to_string()
        })
        .unwrap_or_default()
}

fn ago(at: u64, now: u64) -> String {
    let s = now.saturating_sub(at);
    if s < 3600 {
        "Last used in the last hour".into()
    } else if s < 24 * 3600 {
        format!("Last used {} hours ago", s / 3600)
    } else {
        format!("Last used {}", day(at))
    }
}

/// The page. `may_elevate` offers the "admin rights too" switch, which only
/// an account with elevated permissions can use.
pub fn page(ui: &mut egui::Ui, tokens: &mut AccessTokens, may_elevate: bool) {
    tokens.drain();
    if !tokens.asked {
        tokens.asked = true;
        tokens.refresh();
    }
    theme::note(
        ui,
        "What a bot or a script signs in with instead of your password. Revoking one stops \
         it at once, also a bot already in the world.",
    );
    ui.add_space(6.0);

    if let Some((label, secret)) = tokens.shown.clone() {
        let mut done = false;
        egui::Frame::NONE
            .fill(egui::Color32::from_rgb(0x16, 0x24, 0x1D))
            .stroke(egui::Stroke::new(
                1.0,
                egui::Color32::from_rgb(0x2F, 0x5A, 0x45),
            ))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(egui::Margin::symmetric(14, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let green = egui::Color32::from_rgb(0xC9, 0xF0, 0xDA);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "{label} is ready. Copy it now: it is shown only this once."
                        ))
                        .size(12.5)
                        .color(egui::Color32::from_rgb(0x9F, 0xD3, 0xB4)),
                    )
                    .wrap(),
                );
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(egui::RichText::new(&secret).monospace().color(green))
                        .wrap()
                        .selectable(true),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if theme::action_button(ui, format!("{}  Copy", icons::COPY)).clicked() {
                        ui.ctx().copy_text(secret.clone());
                    }
                    if theme::action_button(ui, "Done").clicked() {
                        done = true;
                    }
                });
            });
        if done {
            tokens.shown = None;
        }
        ui.add_space(8.0);
    }

    if let Some(e) = tokens.error.clone() {
        if theme::inline_error(ui, &e) {
            tokens.error = None;
        }
    }

    let now = now_s();
    match tokens.list.clone() {
        Some(Ok(list)) if !list.is_empty() => {
            theme::list_group(ui, |ui| {
                for t in &list {
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(&t.label).size(14.5).color(theme::CREAM),
                                );
                                if t.elevated {
                                    ui.label(
                                        egui::RichText::new("admin").size(11.0).color(theme::WARN),
                                    );
                                }
                            });
                            let expires = match t.expires_at {
                                Some(at) if at <= now => "expired".to_string(),
                                Some(at) => format!("expires {}", day(at)),
                                None => "never expires".to_string(),
                            };
                            let used = t
                                .last_used
                                .map_or_else(|| "Never used".to_string(), |at| ago(at, now));
                            ui.label(
                                egui::RichText::new(format!(
                                    "Made {} · {expires}\n{used}",
                                    day(t.created_at)
                                ))
                                .size(12.0)
                                .color(theme::TEXT_DIM),
                            );
                            if theme::confirm_button(
                                ui,
                                egui::Id::new(("token_revoke", &t.id)),
                                "Revoke…",
                                "Revoke: it stops working now",
                            ) {
                                tokens.revoke(t);
                            }
                        });
                }
            });
        }
        Some(Ok(_)) => theme::note(ui, "You have no access tokens."),
        Some(Err(e)) => theme::note(ui, &format!("Could not load your tokens: {e}")),
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                theme::note(ui, "Loading…");
            });
        }
    }

    theme::section(ui, "New token");
    crate::systems::text_touch::field(
        ui,
        egui::TextEdit::singleline(&mut tokens.label)
            .id(egui::Id::new("token_label"))
            .desired_width(ui.available_width())
            .hint_text("Name, e.g. \"deck bot\""),
    );
    ui.add_space(4.0);
    theme::segmented(
        ui,
        &mut tokens.expiry,
        &[
            (Expiry::Day, "1 day"),
            (Expiry::Month, "30 days"),
            (Expiry::Never, "Never"),
        ],
    );
    if may_elevate {
        ui.add_space(4.0);
        theme::checkbox(
            ui,
            &mut tokens.elevated,
            "Admin rights too: it may do what your admin roles allow",
        );
    }
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("Your password, to confirm")
            .size(theme::SIZE_SUB)
            .color(theme::TEXT_DIM),
    );
    theme::password_field(ui, &mut tokens.password);
    ui.add_space(6.0);
    let ready = !tokens.label.trim().is_empty() && !tokens.password.is_empty() && !tokens.busy;
    let why = if tokens.busy {
        "Making it…"
    } else {
        "Give it a name and your password"
    };
    if theme::action_enabled_because(ui, ready, "Make token", why).clicked() && ready {
        tokens.make();
    }
}

/// Stand-in tokens for a scripted capture (`--ui tokens`).
pub fn seed_demo(tokens: &mut AccessTokens) {
    tokens.demo = true;
    let now = now_s();
    tokens.list = Some(Ok(vec![
        TokenInfo {
            id: "t1".into(),
            label: "deck bot".into(),
            created_at: now - 5 * 24 * 3600,
            last_used: Some(now - 2 * 3600),
            ..default()
        },
        TokenInfo {
            id: "t2".into(),
            label: "field check script".into(),
            created_at: now - 14 * 24 * 3600,
            expires_at: Some(now + 16 * 24 * 3600),
            ..default()
        },
    ]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_documents_parse() {
        let list: ListDoc = serde_json::from_str(
            r#"{"tokens":[{"id":"a","label":"bot","elevated":false,"created_at":1760000000,
                "last_used":null,"expires_at":null}]}"#,
        )
        .unwrap();
        assert_eq!(list.tokens[0].label, "bot");
        let made: MadeDoc = serde_json::from_str(
            r#"{"token":"cpt_x","info":{"id":"b","label":"x","elevated":true,
                "created_at":1760000000,"last_used":null,"expires_at":1760086400}}"#,
        )
        .unwrap();
        assert_eq!(made.token, "cpt_x");
        assert!(made.info.elevated);
    }

    #[test]
    fn making_a_token_never_keeps_the_password() {
        let mut t = AccessTokens {
            demo: true,
            label: "bot".into(),
            password: "hunter2".into(),
            ..default()
        };
        t.make();
        assert!(t.password.is_empty());
        t.drain();
        assert!(t.shown.is_some());
        t.forget();
        assert!(t.shown.is_none());
    }
}
