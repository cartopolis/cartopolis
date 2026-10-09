//! Full addresses: `name@server` for a person, `server/…` for a conversation
//! (`docs/notes/web-chat.md`, "Federation").
//!
//! Every identity inside an encrypted chat is a full address, never a bare
//! username, and every conversation id begins with its home server — the one
//! that orders its log. Neither is needed while there is one simulator; both
//! are written into things that cannot be changed later (a device's MLS
//! credential lasts as long as the device, a group id as long as the group),
//! so they are decided now, before any server talks to another.
//!
//! A server's name is the host of its public URL (`cartopolis.org`), sent to
//! clients in `ServerHelloAck::server_name`.

/// The separator between a conversation id's home server and its random part.
/// Not a character a host name can contain.
const GROUP_SEP: u8 = b'/';

/// Longest server name accepted, in bytes: a DNS name's limit.
pub const MAX_SERVER_NAME: usize = 253;

/// Whether `s` can be a server name: a host, optionally with a port, and
/// nothing that would make an address or a group id ambiguous.
#[must_use]
pub fn is_server_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_SERVER_NAME
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
}

/// A person's full address: `name` on `server`. Lower-cased, since usernames
/// and host names are both case-insensitive here, and two spellings of one
/// person must compare equal everywhere an address is a key.
#[must_use]
pub fn person(name: &str, server: &str) -> String {
    format!("{}@{}", name.trim().to_lowercase(), server.to_lowercase())
}

/// The name and server of a full address, or `None` for a bare name.
#[must_use]
pub fn split(address: &str) -> Option<(&str, &str)> {
    let (name, server) = address.rsplit_once('@')?;
    (!name.is_empty() && is_server_name(server)).then_some((name, server))
}

/// `who` as a full address: unchanged if it already is one, otherwise a name
/// on `home`. What turns "jeroen" typed into a field into the person on your
/// own server.
#[must_use]
pub fn qualify(who: &str, home: &str) -> String {
    let who = who.trim();
    match split(who) {
        Some((name, server)) => person(name, server),
        None => person(who, home),
    }
}

/// How to show an address to somebody on `home`: the bare name for a person
/// on the same server, the full address for anyone else.
#[must_use]
pub fn display<'a>(address: &'a str, home: &str) -> &'a str {
    match split(address) {
        Some((name, server)) if server.eq_ignore_ascii_case(home) => name,
        _ => address,
    }
}

/// A new conversation id whose home is `server`: the server name, `/`, and
/// sixteen random bytes.
#[must_use]
pub fn group_id(server: &str, random: [u8; 16]) -> Vec<u8> {
    let mut id = server.to_lowercase().into_bytes();
    id.push(GROUP_SEP);
    id.extend_from_slice(&random);
    id
}

/// The home server a conversation id names, if it names one.
#[must_use]
pub fn group_home(id: &[u8]) -> Option<&str> {
    let at = id.iter().position(|&b| b == GROUP_SEP)?;
    let server = std::str::from_utf8(&id[..at]).ok()?;
    is_server_name(server).then_some(server)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_name_is_qualified_with_the_home_server() {
        assert_eq!(qualify("Jeroen", "cartopolis.org"), "jeroen@cartopolis.org");
        assert_eq!(
            qualify("sanne@Other.example", "cartopolis.org"),
            "sanne@other.example"
        );
        assert_eq!(split("jeroen"), None);
        assert_eq!(
            split("jeroen@cartopolis.org"),
            Some(("jeroen", "cartopolis.org"))
        );
    }

    #[test]
    fn a_neighbour_is_shown_by_name_and_a_stranger_in_full() {
        assert_eq!(display("jeroen@cartopolis.org", "CARTOPOLIS.org"), "jeroen");
        assert_eq!(
            display("sanne@other.example", "cartopolis.org"),
            "sanne@other.example"
        );
    }

    #[test]
    fn a_group_id_names_its_home() {
        let id = group_id("cartopolis.org", [0x2f; 16]);
        assert_eq!(group_home(&id), Some("cartopolis.org"));
        assert_eq!(group_home(&[1, 2, 3]), None);
        assert_eq!(group_home(b"bad host/xyz"), None);
    }

    #[test]
    fn a_server_name_cannot_smuggle_a_separator() {
        assert!(is_server_name("localhost:3100"));
        assert!(!is_server_name("a/b"));
        assert!(!is_server_name("a@b"));
        assert!(!is_server_name(""));
    }
}
