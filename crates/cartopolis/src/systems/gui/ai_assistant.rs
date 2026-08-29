use crate::components::{Prim, Region};
use crate::resources::{
    AiAssistantState, AiConfig, DisplayMessage, OnlineSession, PendingAiResponse,
};
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;
use bevy::prelude::*;
use cartopolis_core::{NetMessage, PrimSurface};
use std::sync::{Arc, Mutex};

#[cfg(not(target_arch = "wasm32"))]
const SYSTEM_PROMPT: &str = r#"You are the in-world building assistant for Cartopolis, a 3D world editor. You construct
scenes by composing primitives ("prims") with the create_prim, update_prim, delete_prim,
and list_prims tools. Your goal is believable, well-proportioned architecture assembled
from many parts — never a single bare box.

GEOMETRY
- Right-handed coordinates, Y is up, units are meters.
- A prim's `position` is its CENTER. `scale` is its full size along each axis
  (x = width, y = height, z = depth).
- Rest a prim on the ground (the y=0 plane): position.y = scale.y / 2.
- Stack prim B on top of prim A: B.position.y = (A.position.y + A.scale.y/2) + B.scale.y/2.
- Colors are r,g,b in [0.0, 1.0]. `texture_id` is a catalog filename stem
  (e.g. "concrete-01"); omit it for a flat color.

SHAPES AND THEIR ARCHITECTURAL ROLES
- box: walls, floor slabs, massing blocks, podiums, cornices, mullions, parapets, steps.
- cylinder: columns, round towers, chimneys, silos, the drum under a dome.
- cone: spires, pointed or conical roofs, finials, turret caps.
- sphere: domes and orbs (flatten it, scale.y < scale.x/scale.z, for a shallow dome).
- torus: rings, balcony rails, decorative bands.

SHAPE MODIFIERS (the optional `geometry` field on create_prim/update_prim)
These reshape a single prim along its height (path) or cross-section (profile) so
you can build forms a plain box cannot. Use them deliberately; leave them at default
for ordinary masses.
- taper (taper_x / taper_y, -1..1): shrink the top (+) or bottom (-). Turn a box into
  a pyramid or wedge, a cylinder into a cone, or batter a tower's walls slightly
  (e.g. taper 0.1) for a solid, grounded look. Great for spires, obelisks, ziggurats.
- slice (slice_begin / slice_end, 0..1): trim the prim along its height. Slice a sphere
  from the bottom for a dome or cupola; take a band out of a cylinder.
- path_cut (path_cut_begin / path_cut_end, 0..1): cut an angular wedge through the
  vertical axis. Make a quarter/half cylinder for a corner tower, or an open arcade.
- hollow (0..0.95): turn a prim into a shell or tube — chimney flues, wells, ring walls,
  archways (a hollowed, path-cut cylinder), window/door reveals.
- twist (twist_begin / twist_end, degrees): rotate the profile up the height for a
  spiral or twisted-tower motif. Use sparingly.
- top_shear (top_shear_x / top_shear_y, -0.5..0.5): lean a mass sideways for a buttress,
  a flying form, or a parallelogram.
Modifiers compose (e.g. a tapered + sliced sphere = a ribbed dome segment), and they
respect the prim's position/scale, so a tapered box still rests on the ground.

MAKING A BUILDING LOOK GOOD
1. Massing first. Build every structure from several prims. Favor a tripartite scheme:
   a wider base/podium, a taller shaft/body, and a distinct cap or roof. Step the
   building back as it rises.
2. Human proportion. A floor is ~3m tall, doors ~2-2.5m, windows ~1.5m. Size masses in
   whole-floor multiples and vary width, depth, and height — avoid perfect cubes.
3. Align, never float. Every prim must rest on the ground or sit flush on another prim.
   Stacked pieces should share a face; let trim overlap its host by ~0.05m so no seam
   shows. Avoid deep intersections, gaps, and hovering pieces.
4. Layer detail. After the main masses, add thin boxes for a plinth at the base, string
   courses/cornices between floors, a roof slab or parapet on top, and pilasters or
   columns for vertical rhythm. Small repeated details sell the scale.
5. Rhythm and symmetry. Repeat columns, windows, and bays at a constant spacing, and
   keep the building symmetric about its main axis unless asked otherwise.
6. Cohesive palette. Choose 2-3 colors: a darker base, a mid-tone body, a lighter trim,
   with roofs darker or accented. Put textures on large wall surfaces. Stay restrained.

ROOFS
- Flat/modern: a thin roof slab plus a low parapet box around the edge.
- Pitched: a cone over a round tower, or two boxes leaned into a gable over a square plan.
- Domed: a flattened sphere set on a short cylindrical drum.

WORKFLOW
- Plan the prim list and their stacked Y positions before creating anything; do the
  arithmetic so parts meet exactly.
- Call list_prims first to see what exists, avoid collisions, and build near the existing
  geometry's coordinates rather than at a random origin.
- For several buildings, space them apart by more than their footprint (leave streets)
  and vary height, width, and color so they read as distinct.
- Build one structure's prims, then stop and briefly describe what you made.

Quality bar: a lone box, a floating piece, a uniform grid of identical cubes, or a
clashing palette is a failure — revise before finishing."#;

#[cfg(not(target_arch = "wasm32"))]
fn tools_schema() -> serde_json::Value {
    // Optional procedural shape modifiers, shared by create_prim and update_prim.
    // Every field defaults to a no-op, so a flat box/sphere/etc. is unaffected.
    let geometry = serde_json::json!({
        "type": "object",
        "description": "Optional procedural shape modifiers; omit any field to leave it at its no-op default. The path runs bottom→top along the prim's local Y (height); the profile is its cross-section.",
        "properties": {
            "path_cut_begin": {"type": "number", "description": "0..1, start of an angular wedge cut around the vertical axis (begin < end). Box→wedge, cylinder→pie slice. Default 0."},
            "path_cut_end": {"type": "number", "description": "0..1, end of the angular cut. Default 1 (no cut)."},
            "hollow": {"type": "number", "description": "0..0.95, hollows the prim into a tube/shell (inner size = outer × hollow). Default 0."},
            "twist_begin": {"type": "number", "description": "Degrees, -360..360. Rotation of the cross-section at the bottom. Default 0."},
            "twist_end": {"type": "number", "description": "Degrees, -360..360. Rotation of the cross-section at the top; differ from twist_begin to spiral. Default 0."},
            "taper_x": {"type": "number", "description": "-1..1. +x shrinks the top in width, -x shrinks the bottom. +1 tapers the top to nothing (box→pyramid/wedge, cylinder→cone). Default 0."},
            "taper_y": {"type": "number", "description": "-1..1, taper along depth (Z). Default 0."},
            "top_shear_x": {"type": "number", "description": "-0.5..0.5, slides the top sideways in X relative to the bottom (a lean/parallelogram). Default 0."},
            "top_shear_y": {"type": "number", "description": "-0.5..0.5, top shear along depth (Z). Default 0."},
            "slice_begin": {"type": "number", "description": "0..1, trims the prim along its height/path, keeping [begin, end] (begin < end). A sphere sliced from the bottom becomes a dome. Default 0."},
            "slice_end": {"type": "number", "description": "0..1, end of the slice. Default 1 (no trim)."}
        }
    });
    serde_json::json!([
        {
            "name": "list_prims",
            "description": "List all prims currently in the scene with their properties.",
            "input_schema": {"type": "object", "properties": {}}
        },
        {
            "name": "create_prim",
            "description": "Create a new 3D primitive in the scene.",
            "input_schema": {
                "type": "object",
                "required": ["name", "shape", "position", "scale", "color"],
                "properties": {
                    "name": {"type": "string"},
                    "shape": {"type": "string", "enum": ["box", "sphere", "cylinder", "cone", "torus"]},
                    "position": {
                        "type": "object",
                        "required": ["x", "y", "z"],
                        "properties": {
                            "x": {"type": "number"},
                            "y": {"type": "number", "description": "Vertical (up) axis. Use scale.y/2 to rest on ground."},
                            "z": {"type": "number"}
                        }
                    },
                    "scale": {
                        "type": "object",
                        "description": "Full size in meters (x=width, y=height, z=depth); the prim is centered on position.",
                        "required": ["x", "y", "z"],
                        "properties": {
                            "x": {"type": "number"},
                            "y": {"type": "number"},
                            "z": {"type": "number"}
                        }
                    },
                    "color": {
                        "type": "object",
                        "required": ["r", "g", "b"],
                        "properties": {
                            "r": {"type": "number", "description": "[0.0-1.0]"},
                            "g": {"type": "number", "description": "[0.0-1.0]"},
                            "b": {"type": "number", "description": "[0.0-1.0]"}
                        }
                    },
                    "texture_id": {
                        "type": "string",
                        "description": "Optional texture id (filename stem, e.g. 'brick'). Omit for solid color."
                    },
                    "geometry": geometry.clone()
                }
            }
        },
        {
            "name": "update_prim",
            "description": "Update an existing prim's properties. Only include the fields you want to change.",
            "input_schema": {
                "type": "object",
                "required": ["prim_id"],
                "properties": {
                    "prim_id": {"type": "integer"},
                    "name": {"type": "string"},
                    "shape": {"type": "string", "enum": ["box", "sphere", "cylinder", "cone", "torus"]},
                    "position": {
                        "type": "object",
                        "properties": {
                            "x": {"type": "number"},
                            "y": {"type": "number"},
                            "z": {"type": "number"}
                        }
                    },
                    "scale": {
                        "type": "object",
                        "properties": {
                            "x": {"type": "number"},
                            "y": {"type": "number"},
                            "z": {"type": "number"}
                        }
                    },
                    "color": {
                        "type": "object",
                        "properties": {
                            "r": {"type": "number"},
                            "g": {"type": "number"},
                            "b": {"type": "number"}
                        }
                    },
                    "texture_id": {
                        "type": "string",
                        "description": "Optional texture id (filename stem, e.g. 'brick'). Set to null to clear."
                    },
                    "geometry": geometry.clone()
                }
            }
        },
        {
            "name": "delete_prim",
            "description": "Delete a prim from the scene.",
            "input_schema": {
                "type": "object",
                "required": ["prim_id"],
                "properties": {
                    "prim_id": {"type": "integer"}
                }
            }
        }
    ])
}

fn fire_request(
    api_key: String,
    model: String,
    messages: Vec<serde_json::Value>,
) -> PendingAiResponse {
    let cell: PendingAiResponse = Arc::new(Mutex::new(None));
    // The Anthropic call holds a server secret and is admin-only + server-side on
    // the web build (ADR-034); native calls it directly on a worker thread, wasm
    // returns an unavailable result.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let cell_clone = cell.clone();
        std::thread::spawn(move || {
            let result = call_anthropic(&api_key, &model, &messages);
            *cell_clone.lock().unwrap() = Some(result);
        });
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (&api_key, &model, &messages);
        if let Ok(mut g) = cell.lock() {
            *g = Some(Err(
                "AI assistant is not available on the web build".to_string()
            ));
        }
    }
    cell
}

#[cfg(not(target_arch = "wasm32"))]
fn call_anthropic(
    api_key: &str,
    model: &str,
    messages: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 4096,
        "system": SYSTEM_PROMPT,
        "tools": tools_schema(),
        "messages": messages,
    });
    let body_str = serde_json::to_string(&body).map_err(|e| e.to_string())?;

    // The shared agent, not `ureq::post`: a conversation is one request per turn
    // plus one per tool round-trip, and a private pool per call would hand every
    // one of them a fresh TLS handshake.
    let req = crate::platform::http::native_agent()
        .post("https://api.anthropic.com/v1/messages")
        .set("x-api-key", api_key)
        .set("anthropic-version", "2023-06-01")
        .set("content-type", "application/json");

    let resp_str = match req.send_string(&body_str) {
        Ok(resp) => resp.into_string().map_err(|e| e.to_string())?,
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp
                .into_string()
                .unwrap_or_else(|_| "unknown HTTP error".to_string());
            return Err(format!("HTTP {code}: {body}"));
        }
        Err(e) => return Err(e.to_string()),
    };

    serde_json::from_str(&resp_str).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// UI system
// ---------------------------------------------------------------------------

pub fn render_ai_panel(
    mut egui: ResMut<EguiManager>,
    mut state: ResMut<AiAssistantState>,
    config: Res<AiConfig>,
    mut right_panel: ResMut<crate::resources::RightPanel>,
) {
    crate::systems::egui_manager::right_panel(
        &mut egui,
        &mut right_panel,
        crate::resources::RightPanel::Assistant,
        "ai_panel",
        320.0,
        |ui| {
            if config.api_key.is_none() {
                ui.colored_label(theme::WARN, "Set ANTHROPIC_API_KEY in .env to enable.");
                return;
            }

            let is_busy = state.pending.is_some();

            // Chat history
            let input_height = 80.0;
            let avail = ui.available_height() - input_height - 16.0;
            egui::ScrollArea::vertical()
                .max_height(avail.max(40.0))
                .auto_shrink([false; 2])
                .stick_to_bottom(state.scroll_to_bottom)
                .show(ui, |ui| {
                    for msg in &state.display_messages {
                        if msg.is_user {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                ui.group(|ui| {
                                    ui.set_max_width(ui.available_width() * 0.85);
                                    ui.label(&msg.text);
                                });
                            });
                        } else {
                            ui.group(|ui| {
                                ui.set_max_width(ui.available_width());
                                ui.label(&msg.text);
                            });
                        }
                        ui.add_space(4.0);
                    }
                    if is_busy {
                        theme::busy(ui, "Thinking…");
                    }
                });

            state.scroll_to_bottom = false;

            if let Some(err) = &state.error.clone() {
                ui.colored_label(theme::ERROR, err);
            }

            ui.separator();

            // Input row
            ui.add_enabled_ui(!is_busy, |ui| {
                let resp = ui.add(
                    egui::TextEdit::multiline(&mut state.input)
                        .desired_rows(3)
                        .hint_text("Ask the AI to create or edit prims… (Ctrl+Enter)"),
                );

                let submit = ui.button("Send").clicked()
                    || (resp.has_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl));

                if submit && !state.input.trim().is_empty() {
                    state.error = None;
                    state.pending_send = true;
                }
            });
        },
    );
}

// ---------------------------------------------------------------------------
// Response polling + tool execution
// ---------------------------------------------------------------------------

pub fn poll_ai_response(
    mut state: ResMut<AiAssistantState>,
    config: Res<AiConfig>,
    online: Option<Res<OnlineSession>>,
    prim_query: Query<(Entity, &Prim, &Transform)>,
    region_query: Query<&Region>,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    // Fire request when the user submitted a message
    if state.pending_send && state.pending.is_none() {
        state.pending_send = false;
        let text = state.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        state.input.clear();
        state.display_messages.push(DisplayMessage {
            is_user: true,
            text: text.clone(),
        });
        state
            .api_messages
            .push(serde_json::json!({"role": "user", "content": text}));
        state.scroll_to_bottom = true;
        state.tool_round = 0;

        if let Some(key) = config.api_key.clone() {
            state.pending = Some(fire_request(
                key,
                config.model.clone(),
                state.api_messages.clone(),
            ));
        }
        return;
    }

    // Poll for response
    let response = {
        let Some(cell) = &state.pending else {
            return;
        };
        let mut guard = cell.lock().unwrap();
        guard.take()
    };
    let Some(response) = response else {
        return;
    };
    state.pending = None;

    match response {
        Err(e) => {
            tracing::error!("Anthropic API error: {}", e);
            state.error = Some(format!("Error: {e}"));
        }
        Ok(json) => {
            // Surface API-level errors (e.g. bad auth key)
            if json.get("error").is_some() {
                let msg = json["error"]["message"]
                    .as_str()
                    .unwrap_or("unknown API error")
                    .to_string();
                state.error = Some(format!("API error: {msg}"));
                return;
            }

            // Store assistant message in history (role + content only)
            state.api_messages.push(serde_json::json!({
                "role": "assistant",
                "content": json["content"],
            }));

            let Some(content) = json["content"].as_array() else {
                return;
            };

            let mut text_parts: Vec<String> = Vec::new();
            let mut tool_calls: Vec<serde_json::Value> = Vec::new();

            for block in content {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(t) = block["text"].as_str() {
                            text_parts.push(t.to_string());
                        }
                    }
                    Some("tool_use") => {
                        tool_calls.push(block.clone());
                    }
                    _ => {}
                }
            }

            if !text_parts.is_empty() {
                state.display_messages.push(DisplayMessage {
                    is_user: false,
                    text: text_parts.join("\n"),
                });
                state.scroll_to_bottom = true;
            }

            if tool_calls.is_empty() {
                return; // conversation turn complete
            }

            if state.tool_round >= 8 {
                state.error = Some("Reached tool-call limit (8 rounds).".to_string());
                return;
            }
            state.tool_round += 1;

            // Execute tools and collect results
            let mut tool_results: Vec<serde_json::Value> = Vec::new();
            for call in &tool_calls {
                let tool_id = call["id"].as_str().unwrap_or("").to_string();
                let tool_name = call["name"].as_str().unwrap_or("").to_string();
                let input = &call["input"];

                let result_text = run_tool(
                    &tool_name,
                    input,
                    online.as_deref(),
                    &prim_query,
                    &region_query,
                    &map_anchor,
                );

                state.display_messages.push(DisplayMessage {
                    is_user: false,
                    // `»`, not `→` (and not `➡`, which is equally absent): no
                    // arrow in any block is in the bundled stack. Latin-1 is the
                    // only guarantee without a `Context` to probe against.
                    text: format!("[ {} » {} ]", tool_name, result_text),
                });
                state.scroll_to_bottom = true;

                tool_results.push(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tool_id,
                    "content": result_text,
                }));
            }

            // Feed tool results back and fire next request
            state
                .api_messages
                .push(serde_json::json!({"role": "user", "content": tool_results}));

            if let Some(key) = config.api_key.clone() {
                state.pending = Some(fire_request(
                    key,
                    config.model.clone(),
                    state.api_messages.clone(),
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tool execution
// ---------------------------------------------------------------------------

fn run_tool(
    name: &str,
    input: &serde_json::Value,
    online: Option<&OnlineSession>,
    prim_query: &Query<(Entity, &Prim, &Transform)>,
    region_query: &Query<&Region>,
    map_anchor: &crate::resources::MapAnchor,
) -> String {
    match name {
        "list_prims" => {
            let list: Vec<serde_json::Value> = prim_query
                .iter()
                .map(|(_, p, t)| {
                    let lin = p.color.to_linear();
                    serde_json::json!({
                        "id": p.id,
                        "name": p.name,
                        "shape": format!("{:?}", p.shape).to_lowercase(),
                        "position": {"x": t.translation.x, "y": t.translation.y, "z": t.translation.z},
                        "scale":    {"x": t.scale.x,       "y": t.scale.y,       "z": t.scale.z},
                        "color":    {"r": lin.red,          "g": lin.green,       "b": lin.blue},
                    })
                })
                .collect();
            serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string())
        }

        "create_prim" => {
            let prim_name = input["name"].as_str().unwrap_or("Prim").to_string();
            let shape_str = input["shape"].as_str().unwrap_or("box").to_string();
            let pos = vec3_from(input, "position");
            let scale = vec3_from(input, "scale");
            let color = color_from(input);
            let texture_id = input["texture_id"].as_str().map(|s| s.to_string());
            let region_id = region_query.iter().next().map(|r| r.id).unwrap_or(1);
            let geom = PrimGeom::defaults().merged_with(input);

            // ADR-029: prims are online-only and geo-anchored. The server assigns
            // the id and echoes a PrimUpsert that spawns the entity locally.
            let Some(session) = online else {
                return "Failed: prims require an online session".to_string();
            };
            let Some(geo) = crate::systems::prim_geo::geo_from_world(pos, &map_anchor) else {
                return "Failed: world not anchored yet".to_string();
            };
            session.send(
                "create prim",
                NetMessage::CreatePrim {
                    request_id: 0,
                    region_id,
                    name: prim_name.clone(),
                    position: pos,
                    rotation: Vec3::ZERO,
                    scale,
                    color,
                    texture_id,
                    shape: shape_str,
                    surface: PrimSurface::default(),
                    geometry: geom.geometry(),
                    geo,
                    // The assistant has no vocabulary for an emitter; a prim it
                    // makes has none.
                    particles: None,
                },
            );
            format!("Created '{}' (sent to server)", prim_name)
        }

        "update_prim" => {
            let prim_id = match input["prim_id"].as_i64() {
                Some(id) => id,
                None => return "Error: missing prim_id".to_string(),
            };

            // Find current values to merge with partial updates
            let existing = prim_query.iter().find(|(_, p, _)| p.id == prim_id);
            let (cur_name, cur_pos, cur_scale, cur_color, cur_geom, cur_texture, cur_surface) =
                match existing {
                    Some((_, p, t)) => {
                        let lin = p.color.to_linear();
                        (
                            p.name.clone(),
                            t.translation,
                            t.scale,
                            [lin.red, lin.green, lin.blue],
                            PrimGeom::from_prim(p),
                            p.texture_id.clone(),
                            p.surface,
                        )
                    }
                    None => return format!("Error: prim {} not found", prim_id),
                };
            let new_geom = cur_geom.merged_with(input);

            let new_name = input["name"].as_str().unwrap_or(&cur_name).to_string();
            // NOTE: UpdatePrim carries no shape field (ADR-017), so an online
            // update can't change a prim's shape — only its transform/material.
            let new_pos = if input.get("position").is_some() {
                vec3_from(input, "position")
            } else {
                cur_pos
            };
            let new_scale = if input.get("scale").is_some() {
                vec3_from(input, "scale")
            } else {
                cur_scale
            };
            let new_color = if input.get("color").is_some() {
                color_from(input)
            } else {
                cur_color
            };
            let new_texture_id = if input.get("texture_id").is_some() {
                input["texture_id"].as_str().map(|s| s.to_string())
            } else {
                cur_texture
            };

            // ADR-029: send the authoritative update; the PrimUpsert echo applies it.
            let Some(session) = online else {
                return "Failed: prims require an online session".to_string();
            };
            let Some(geo) = crate::systems::prim_geo::geo_from_world(new_pos, &map_anchor) else {
                return "Failed: world not anchored yet".to_string();
            };
            session.send(
                "update prim",
                NetMessage::UpdatePrim {
                    request_id: 0,
                    prim_id,
                    position: new_pos,
                    rotation: Vec3::ZERO,
                    scale: new_scale,
                    color: new_color,
                    texture_id: new_texture_id,
                    name: new_name,
                    surface: cur_surface,
                    geometry: new_geom.geometry(),
                    geo,
                    // Carried over, like every other field the tool call did not
                    // mention: `UpdatePrim` replaces the whole prim, so an
                    // unmentioned emitter must be re-sent or it is removed.
                    particles: existing.and_then(|(_, p, _)| p.particles),
                },
            );
            format!("Updated prim {} (sent to server)", prim_id)
        }

        "delete_prim" => {
            let prim_id = match input["prim_id"].as_i64() {
                Some(id) => id,
                None => return "Error: missing prim_id".to_string(),
            };

            let Some(session) = online else {
                return "Failed: prims require an online session".to_string();
            };
            session.send(
                "delete prim",
                NetMessage::DeletePrim {
                    request_id: 0,
                    prim_id,
                },
            );
            format!("Deleted prim {} (sent to server)", prim_id)
        }

        other => format!("Unknown tool: {}", other),
    }
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

fn vec3_from(v: &serde_json::Value, key: &str) -> Vec3 {
    let obj = &v[key];
    Vec3::new(
        obj["x"].as_f64().unwrap_or(0.0) as f32,
        obj["y"].as_f64().unwrap_or(0.0) as f32,
        obj["z"].as_f64().unwrap_or(0.0) as f32,
    )
}

fn color_from(v: &serde_json::Value) -> [f32; 3] {
    let obj = &v["color"];
    [
        obj["r"].as_f64().unwrap_or(0.5) as f32,
        obj["g"].as_f64().unwrap_or(0.5) as f32,
        obj["b"].as_f64().unwrap_or(0.5) as f32,
    ]
}

/// Full procedural-geometry state (path cut, hollow, twist, taper, top shear,
/// slice) shared by create/update. Lets a tool call merge only the fields it
/// supplies over a base (defaults for create, the current prim for update).
#[derive(Clone, Copy)]
struct PrimGeom {
    path_cut_begin: f32,
    path_cut_end: f32,
    hollow: f32,
    twist_begin: f32,
    twist_end: f32,
    taper_x: f32,
    taper_y: f32,
    top_shear_x: f32,
    top_shear_y: f32,
    slice_begin: f32,
    slice_end: f32,
}

impl PrimGeom {
    fn defaults() -> Self {
        Self {
            path_cut_begin: 0.0,
            path_cut_end: 1.0,
            hollow: 0.0,
            twist_begin: 0.0,
            twist_end: 0.0,
            taper_x: 0.0,
            taper_y: 0.0,
            top_shear_x: 0.0,
            top_shear_y: 0.0,
            slice_begin: 0.0,
            slice_end: 1.0,
        }
    }

    fn from_prim(p: &Prim) -> Self {
        Self {
            path_cut_begin: p.path_cut_begin,
            path_cut_end: p.path_cut_end,
            hollow: p.hollow,
            twist_begin: p.twist_begin,
            twist_end: p.twist_end,
            taper_x: p.taper_x,
            taper_y: p.taper_y,
            top_shear_x: p.top_shear_x,
            top_shear_y: p.top_shear_y,
            slice_begin: p.slice_begin,
            slice_end: p.slice_end,
        }
    }

    /// Override fields present in the call's `geometry` object; keep the rest.
    fn merged_with(self, input: &serde_json::Value) -> Self {
        let g = &input["geometry"];
        if !g.is_object() {
            return self;
        }
        let pick = |key: &str, cur: f32| g[key].as_f64().map(|v| v as f32).unwrap_or(cur);
        Self {
            path_cut_begin: pick("path_cut_begin", self.path_cut_begin),
            path_cut_end: pick("path_cut_end", self.path_cut_end),
            hollow: pick("hollow", self.hollow),
            twist_begin: pick("twist_begin", self.twist_begin),
            twist_end: pick("twist_end", self.twist_end),
            taper_x: pick("taper_x", self.taper_x),
            taper_y: pick("taper_y", self.taper_y),
            top_shear_x: pick("top_shear_x", self.top_shear_x),
            top_shear_y: pick("top_shear_y", self.top_shear_y),
            slice_begin: pick("slice_begin", self.slice_begin),
            slice_end: pick("slice_end", self.slice_end),
        }
    }

    /// Build the wire geometry payload (ADR-018) for a Create/UpdatePrim message.
    fn geometry(&self) -> cartopolis_core::PrimGeometry {
        cartopolis_core::PrimGeometry {
            path_cut_begin: self.path_cut_begin,
            path_cut_end: self.path_cut_end,
            hollow: self.hollow,
            twist_begin: self.twist_begin,
            twist_end: self.twist_end,
            taper_x: self.taper_x,
            taper_y: self.taper_y,
            top_shear_x: self.top_shear_x,
            top_shear_y: self.top_shear_y,
            slice_begin: self.slice_begin,
            slice_end: self.slice_end,
        }
    }
}
