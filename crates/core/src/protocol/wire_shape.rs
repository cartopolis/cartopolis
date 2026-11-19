//! The wire's shape, pinned.
//!
//! postcard is not self-describing: a `NetMessage` variant travels as its
//! *position* in the enum and a struct as its fields *in order*. Moving a
//! variant, inserting one mid-enum or reordering two fields still compiles,
//! still round-trips in every test that encodes and decodes with the same
//! build — and makes every deployed peer read garbage. `MessageKind`'s explicit
//! numbers do not guard this; they are the header's, not the payload's.
//!
//! So this traces every type reachable from `NetMessage` (variant order, field
//! order, field types) into text and compares it with `wire_shape.txt`. A diff
//! there is a wire change: append a `PROTOCOL_HISTORY` row, then rewrite the
//! file with `CARTO_BLESS_WIRE=1 cargo test -p cartopolis_core wire_shape` and
//! commit it beside the change.

use super::*;
use serde_reflection::{ContainerFormat, Format, Samples, Tracer, TracerConfig, VariantFormat};

const PINNED: &str = include_str!("wire_shape.txt");

fn traced() -> String {
    // The tracer answers every byte string with an empty one, which a uuid refuses
    // (it wants exactly 16); sixteen zeroes satisfy it and every other byte field.
    let config = TracerConfig::default()
        .default_borrowed_bytes_value(&[0; 16])
        .default_byte_buf_value(vec![0; 16]);
    let mut tracer = Tracer::new(config);
    let samples = Samples::new();
    // The tracer meets a nested enum once, at its first variant. Each enum below
    // is walked on its own first, so the walk of `NetMessage` finds them whole.
    // A new nested enum fails `registry()` with `MissingVariants([..])`, naming
    // it: add it here.
    macro_rules! enums {
        ($($t:ty),* $(,)?) => {$(
            tracer.trace_simple_type::<$t>().expect(stringify!($t));
        )*};
    }
    enums!(
        AuthCredential,
        CallAction,
        CallEndReason,
        CallRole,
        CallTrackKind,
        ChatScope,
        CommunityRole,
        DirectoryOrder,
        Emote,
        Expression,
        crate::greet::Greeting,
        JoinRule,
        ListingState,
        Relation,
        ReportReason,
        RequestPolicy,
        ScriptLevel,
        crate::sounds::SoundName,
        crate::tone::Voice,
    );
    tracer
        .trace_type::<NetMessage>(&samples)
        .expect("trace NetMessage");
    // Not on the wire, but the server keeps looks as postcard bytes (`looks.rs`),
    // so its layout is as load-bearing as a message's.
    tracer
        .trace_type::<AvatarLook>(&samples)
        .expect("trace AvatarLook");
    let registry = tracer.registry().expect("every traced type is complete");
    // `Registry` is a BTreeMap: the text is ordered by type name, so it diffs
    // stably whatever order the types were met in.
    // One line per variant or field, numbered as postcard numbers them, so a
    // review reads a wire change straight off the diff.
    let mut out = String::from("# The wire's shape. Generated; see wire_shape.rs.\n");
    for (name, container) in &registry {
        match container {
            ContainerFormat::UnitStruct => out.push_str(&format!("{name} = unit\n")),
            ContainerFormat::NewTypeStruct(f) => {
                out.push_str(&format!("{name} = ({})\n", ty(f)));
            }
            ContainerFormat::TupleStruct(fs) => {
                out.push_str(&format!("{name} = {}\n", tuple(fs)));
            }
            ContainerFormat::Struct(fields) => {
                out.push_str(&format!("{name} = struct\n"));
                for (i, f) in fields.iter().enumerate() {
                    out.push_str(&format!("  .{i} {}: {}\n", f.name, ty(&f.value)));
                }
            }
            ContainerFormat::Enum(variants) => {
                out.push_str(&format!("{name} = enum\n"));
                for (i, v) in variants {
                    let shape = match &v.value {
                        VariantFormat::Unit => String::new(),
                        VariantFormat::NewType(f) => format!("({})", ty(f)),
                        VariantFormat::Tuple(fs) => tuple(fs),
                        VariantFormat::Struct(fs) => {
                            let fs: Vec<String> = fs
                                .iter()
                                .map(|f| format!("{}: {}", f.name, ty(&f.value)))
                                .collect();
                            format!(" {{ {} }}", fs.join(", "))
                        }
                        VariantFormat::Variable(_) => "<unresolved>".into(),
                    };
                    out.push_str(&format!("  #{i} {}{shape}\n", v.name));
                }
            }
        }
    }
    out
}

fn tuple(fs: &[Format]) -> String {
    let fs: Vec<String> = fs.iter().map(ty).collect();
    format!("({})", fs.join(", "))
}

fn ty(f: &Format) -> String {
    match f {
        Format::TypeName(n) => n.clone(),
        Format::Option(f) => format!("Option<{}>", ty(f)),
        Format::Seq(f) => format!("Vec<{}>", ty(f)),
        Format::Map { key, value } => format!("Map<{}, {}>", ty(key), ty(value)),
        Format::Tuple(fs) => tuple(fs),
        Format::TupleArray { content, size } => format!("[{}; {size}]", ty(content)),
        // Primitives print as the tracer names them: U32, Str, Bytes, …
        other => format!("{other:?}"),
    }
}

#[test]
fn wire_shape_is_pinned() {
    let now = traced();
    if std::env::var_os("CARTO_BLESS_WIRE").is_some() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/protocol/wire_shape.txt");
        std::fs::write(path, &now).expect("write wire_shape.txt");
        return;
    }
    if now == PINNED {
        return;
    }
    let first = now
        .lines()
        .zip(PINNED.lines())
        .position(|(a, b)| a != b)
        .unwrap_or(now.lines().count().min(PINNED.lines().count()));
    panic!(
        "the wire's shape changed (first difference at wire_shape.txt line {}).\n\
         now:    {:?}\n\
         pinned: {:?}\n\
         A reordered variant or field breaks every deployed peer. If the change is \
         meant, append a PROTOCOL_HISTORY row and re-pin with CARTO_BLESS_WIRE=1.",
        first + 1,
        now.lines().nth(first).unwrap_or("<end>"),
        PINNED.lines().nth(first).unwrap_or("<end>"),
    );
}
