//! What the Settings panel reads and writes of the editor link (`mcp`): the
//! listener's state, who an agent may hear, and the line to paste into an
//! editor. Below `mcp` and the panels both, so Settings can show the link
//! without depending on the server that drives it.

use bevy::prelude::*;
use std::path::{Path, PathBuf};

use cartopolis_mcp::server;

/// Where this run would serve, and how, if it serves at all.
///
/// A `Resource`, because whether it *is* serving is decided while the app runs
/// (`AppPrefs::mcp_enabled`, the Settings switch) and [`reconcile_listener`]
/// needs to know where to bind when it is switched back on.
#[derive(Resource, Clone, Debug)]
pub struct Config {
    /// The socket path.
    pub listen: PathBuf,
    /// No window: render offscreen at [`Config::size`] and stay up serving calls.
    pub headless: bool,
    /// The offscreen surface, when there is no window to be the surface.
    pub size: UVec2,
    /// **Serve regardless of the preference.** `--mcp-headless` has no Settings
    /// panel to switch anything from, so the flag is the whole decision; a
    /// `user.json` carried from a desktop with the switch off would otherwise
    /// leave a capture server answering nothing, with no way to say so.
    pub forced: bool,
}

/// A live listener, as a resource; see [`server::Running`], which it wraps
/// because a type from another crate cannot derive `Resource` here. **Dropping
/// it stops the server.**
#[derive(Resource)]
pub struct Running(pub server::Running);

/// Why the client is not serving, when it is not.
///
/// Kept so Settings can say *which* — "you switched it off" and "another client
/// is already serving there" want different things from the reader, and a row that only
/// said "not serving" would send somebody looking for a setting they had not
/// touched.
#[derive(Resource, Default)]
pub struct McpTrouble(pub Option<String>);

/// Everything the Settings panel's Editor access row needs, as one parameter.
///
/// **One rather than three, because Bevy caps a system at sixteen** and
/// `render_settings_panel` was already at thirteen — the same cap
/// `free_camera::camera_controls` and `picking::prim_picking` each hit and
/// answered the same way. Every field is optional: the resources exist only on a
/// build that was asked to serve, and the store is absent in a scripted run.
///
/// The user store is in here, rather than beside it, because the allow-list is
/// store data and the row both shows and edits it.
#[derive(bevy::ecs::system::SystemParam)]
pub struct EditorAccess<'w> {
    pub config: Option<Res<'w, Config>>,
    pub running: Option<Res<'w, Running>>,
    pub trouble: Option<Res<'w, McpTrouble>>,
    pub store: Option<ResMut<'w, crate::systems::user_store::UserStore>>,
    pub audience: Option<ResMut<'w, Audience>>,
}

/// What the Editor access row has to say about the listener right now.
pub enum Serving {
    /// Open, on this socket.
    At(PathBuf),
    /// Switched off here.
    Off,
    /// Wanted, and could not. The string is the reason, written for a reader —
    /// almost always another client already serving on the socket.
    Failed(String),
}

impl EditorAccess<'_> {
    /// Where it is serving, or why it is not.
    pub fn serving(&self) -> Option<Serving> {
        self.config.as_deref()?;
        Some(match (&self.running, &self.trouble) {
            (Some(running), _) => Serving::At(running.0.listen.clone()),
            (None, Some(trouble)) => match &trouble.0 {
                Some(why) => Serving::Failed(why.clone()),
                None => Serving::Off,
            },
            (None, None) => Serving::Off,
        })
    }

    /// Whose chat the agent may read, as the person typed it — the list is
    /// stored raw so a half-typed `jeroen,` keeps its comma — and whether a flag
    /// fixed it for this run.
    pub fn allow(&self) -> (String, bool) {
        match self.audience.as_deref() {
            Some(a) if a.pinned => (a.to_list(), true),
            _ => (
                self.store
                    .as_deref()
                    .and_then(|s| s.data.mcp_allow.clone())
                    .unwrap_or_default(),
                false,
            ),
        }
    }

    /// Change the list, in the store and in the filter at once, so the agent
    /// stops hearing somebody the moment they are taken off.
    pub fn set_allow(&mut self, list: String) {
        if self.audience.as_deref().is_some_and(|a| a.pinned) {
            return;
        }
        if let Some(audience) = self.audience.as_deref_mut() {
            *audience = Audience::parse(&list);
        }
        if let Some(store) = self.store.as_deref_mut() {
            store.data.mcp_allow = Some(list);
            store.dirty = true;
        }
    }
}

/// What to paste into an editor's MCP configuration to reach this client: the
/// relay, by the absolute path of the binary that is running — which is what
/// works for somebody who unpacked a download rather than installing it on
/// `PATH` — plus `--socket` when the client is not serving on the default.
pub fn editor_config(exe: &Path, listen: &Path, default: &Path) -> String {
    let mut args = vec!["mcp".to_string()];
    if listen != default {
        args.push("--socket".into());
        args.push(listen.display().to_string());
    }
    serde_json::json!({
        "command": exe.display().to_string(),
        "args": args,
    })
    .to_string()
}

/// [`editor_config`] for this process: the running binary, and the default
/// path the relay would look at. Falls back to a bare `cartopolis` only if the
/// OS will not say where the binary is, which `PATH` may still answer.
pub fn editor_config_here(listen: &Path) -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cartopolis"));
    editor_config(&exe, listen, &server::default_socket_path())
}

/// Whose words an agent may read. See the module note.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct Audience {
    pub names: Vec<String>,
    /// `*`: everyone. Offered because it is sometimes what somebody wants — a
    /// greeter bot in a public square — and refused by nothing, but it is the
    /// one setting under which a stranger can put words in front of the model.
    pub everyone: bool,
    /// Set by `--mcp-allow`: the flag is an instruction for this launch, and
    /// Settings shows the list without being able to change it.
    pub pinned: bool,
    /// Set by `--mcp-private-only`: the agent speaks in private chats and
    /// nowhere else. `say` without `to` is refused, and so is a script that
    /// could make one of its objects speak ([`public_speech_in`]). Here, below
    /// the agent, because an instruction in the agent's brief is only advice.
    pub private_only: bool,
}
