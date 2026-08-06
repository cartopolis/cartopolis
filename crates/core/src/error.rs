#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("postcard decode: {0}")]
    Decode(#[from] postcard::Error),
    #[error("postcard encode: {0}")]
    Encode(postcard::Error),
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u16),
    #[error("expected ClientHello first, got {0:?}")]
    ExpectedHello(Box<str>),
    #[error("session not authenticated")]
    NotAuthenticated,
    #[error("app frame too short: {0} bytes")]
    FrameTooShort(usize),
    #[error("unknown message kind {0}")]
    UnknownMessageKind(u16),
    #[error("message kind header/body mismatch: header {header} body {body}")]
    KindMismatch { header: u16, body: u16 },
}

impl ProtocolError {
    pub fn encode(e: postcard::Error) -> Self {
        ProtocolError::Encode(e)
    }
}
