//! The OpenAI Realtime API's events, as JSON: the ones this bridge sends and
//! the ones it reads. Pure, so the wording of the protocol is tested apart from
//! the socket.
//!
//! Written against the generally available API (2025-08). Where the beta named
//! an event differently (`response.audio.delta` for `response.output_audio.delta`,
//! `response.text.*` for `response.output_text.*`) both are read, so a model
//! still on the beta's names does not go silent.

use base64::Engine as _;
use serde_json::{json, Value};

use crate::ToolSpec;

/// The rate both directions run at on the model's side.
pub const MODEL_RATE: u32 = 24_000;

/// The session: who the agent is, how it sounds, how turns are taken and what
/// it may call.
#[must_use]
pub fn session_update(instructions: &str, voice: &str, tools: &[ToolSpec]) -> Value {
    let tools: Vec<Value> = tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            })
        })
        .collect();
    json!({
        "type": "session.update",
        "session": {
            "type": "realtime",
            "output_modalities": ["audio"],
            "instructions": instructions,
            "audio": {
                "input": {
                    "format": { "type": "audio/pcm", "rate": MODEL_RATE },
                    // The model's own turn-taking: it answers when the caller
                    // stops, and stops when the caller starts.
                    "turn_detection": {
                        "type": "server_vad",
                        "create_response": true,
                        "interrupt_response": true,
                        "silence_duration_ms": 500,
                    },
                    // No transcription: voice stays voice.
                    "transcription": null,
                },
                "output": {
                    "format": { "type": "audio/pcm", "rate": MODEL_RATE },
                    "voice": voice,
                },
            },
            "tools": tools,
            "tool_choice": "auto",
        },
    })
}

/// Caller audio at [`MODEL_RATE`], PCM16 little-endian.
#[must_use]
pub fn append_audio(pcm16: &[u8]) -> Value {
    json!({
        "type": "input_audio_buffer.append",
        "audio": base64::engine::general_purpose::STANDARD.encode(pcm16),
    })
}

/// "Your turn": the model answers what the conversation holds so far.
#[must_use]
pub fn respond() -> Value {
    json!({ "type": "response.create" })
}

/// A reply in text only, with instructions for this one reply: the line an
/// outbound call leaves for whoever asked for it.
#[must_use]
pub fn respond_in_text(instructions: &str) -> Value {
    json!({
        "type": "response.create",
        "response": {
            "output_modalities": ["text"],
            "instructions": instructions,
            "tool_choice": "none",
        },
    })
}

/// The model's reply was cut off after `audio_end_ms` of it was played.
#[must_use]
pub fn truncate(item_id: &str, audio_end_ms: u32) -> Value {
    json!({
        "type": "conversation.item.truncate",
        "item_id": item_id,
        "content_index": 0,
        "audio_end_ms": audio_end_ms,
    })
}

/// A tool's answer.
#[must_use]
pub fn function_output(call_id: &str, output: &str) -> Value {
    json!({
        "type": "conversation.item.create",
        "item": {
            "type": "function_call_output",
            "call_id": call_id,
            "output": output,
        },
    })
}

/// A line put into the conversation by the bridge (who started speaking,
/// that a walk ended): a system message, so the model does not take it for
/// something the caller said.
#[must_use]
pub fn system_note(text: &str) -> Value {
    json!({
        "type": "conversation.item.create",
        "item": {
            "type": "message",
            "role": "system",
            "content": [{ "type": "input_text", "text": text }],
        },
    })
}

/// A picture a tool returned, put in front of the model.
#[must_use]
pub fn image(jpeg_base64: &str) -> Value {
    json!({
        "type": "conversation.item.create",
        "item": {
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_image",
                "image_url": format!("data:image/jpeg;base64,{jpeg_base64}"),
            }],
        },
    })
}

/// What the server said, reduced to what the bridge acts on.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    /// The session is configured.
    Ready,
    /// More of the reply's audio, PCM16 at [`MODEL_RATE`].
    Audio { item: String, pcm16: Vec<u8> },
    /// The whole of a text reply.
    Text(String),
    /// The model wants a tool.
    Call {
        call_id: String,
        name: String,
        arguments: String,
    },
    /// The caller started talking.
    SpeechStarted,
    ResponseStarted,
    ResponseDone,
    Error(String),
    /// Anything else: nothing to do.
    Other,
}

#[must_use]
pub fn parse(text: &str) -> Incoming {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Incoming::Other;
    };
    let s = |key: &str| v[key].as_str().unwrap_or_default().to_string();
    match v["type"].as_str().unwrap_or_default() {
        "session.updated" => Incoming::Ready,
        "response.output_audio.delta" | "response.audio.delta" => {
            match base64::engine::general_purpose::STANDARD.decode(s("delta")) {
                Ok(pcm16) => Incoming::Audio {
                    item: s("item_id"),
                    pcm16,
                },
                Err(_) => Incoming::Other,
            }
        }
        "response.output_text.done" | "response.text.done" => Incoming::Text(s("text")),
        // The finished item, not `response.function_call_arguments.done`: the
        // beta's version of that event carried no tool name.
        "response.output_item.done" if v["item"]["type"] == "function_call" => {
            let item = &v["item"];
            Incoming::Call {
                call_id: item["call_id"].as_str().unwrap_or_default().to_string(),
                name: item["name"].as_str().unwrap_or_default().to_string(),
                arguments: item["arguments"].as_str().unwrap_or("{}").to_string(),
            }
        }
        "input_audio_buffer.speech_started" => Incoming::SpeechStarted,
        "response.created" => Incoming::ResponseStarted,
        "response.done" => Incoming::ResponseDone,
        "error" => Incoming::Error(
            v["error"]["message"]
                .as_str()
                .unwrap_or("an error with no message")
                .to_string(),
        ),
        _ => Incoming::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_offers_each_tool_as_a_function() {
        let tools = [ToolSpec {
            name: "look_around".into(),
            description: "Describe the surroundings.".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }];
        let v = session_update("Be brief.", "marin", &tools);
        assert_eq!(v["session"]["tools"][0]["type"], "function");
        assert_eq!(v["session"]["tools"][0]["name"], "look_around");
        assert_eq!(v["session"]["audio"]["input"]["format"]["rate"], 24_000);
        assert_eq!(v["session"]["audio"]["output"]["voice"], "marin");
        assert!(v["session"]["audio"]["input"]["transcription"].is_null());
    }

    #[test]
    fn audio_reads_under_both_names() {
        for kind in ["response.output_audio.delta", "response.audio.delta"] {
            let msg = json!({"type": kind, "item_id": "it_1", "delta": "AAECAw=="}).to_string();
            assert_eq!(
                parse(&msg),
                Incoming::Audio {
                    item: "it_1".into(),
                    pcm16: vec![0, 1, 2, 3]
                }
            );
        }
    }

    #[test]
    fn a_finished_function_call_item_is_a_tool_call() {
        let msg = json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "call_9",
                "name": "walk_to",
                "arguments": "{\"place\":\"home\"}",
            },
        })
        .to_string();
        assert_eq!(
            parse(&msg),
            Incoming::Call {
                call_id: "call_9".into(),
                name: "walk_to".into(),
                arguments: "{\"place\":\"home\"}".into(),
            }
        );
        // A finished message item is not a call.
        let msg = json!({"type": "response.output_item.done", "item": {"type": "message"}});
        assert_eq!(parse(&msg.to_string()), Incoming::Other);
    }

    #[test]
    fn an_error_keeps_its_message() {
        let msg = json!({"type": "error", "error": {"message": "bad key"}}).to_string();
        assert_eq!(parse(&msg), Incoming::Error("bad key".into()));
        assert_eq!(parse("not json"), Incoming::Other);
    }
}
