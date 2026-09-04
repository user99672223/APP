//! `app://` deep links. Built by the server (ntfy click URL) and parsed by the app.
//!
//! - `app://call/<callId>?from=<userId>&exp=<unixtime>`
//! - `app://dm/<userId>?msg=<msgId>`
//! - `app://room/<roomId>`

use crate::ids::{CallId, MessageId, RoomId, UserId};
use std::fmt;
use std::str::FromStr;

pub const SCHEME: &str = "app";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeepLink {
    /// `exp` is unix time in seconds after which the app must not ring.
    Call {
        call_id: CallId,
        from: UserId,
        exp: u64,
    },
    Dm {
        user_id: UserId,
        msg: Option<MessageId>,
    },
    Room {
        room_id: RoomId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DeepLinkError {
    #[error("not an app:// link")]
    Scheme,
    #[error("unknown link path")]
    Path,
    #[error("missing parameter {0}")]
    Missing(&'static str),
    #[error("bad number in {0}")]
    Number(&'static str),
}

impl DeepLink {
    pub fn to_url(&self) -> String {
        match self {
            DeepLink::Call { call_id, from, exp } => {
                format!("{SCHEME}://call/{call_id}?from={from}&exp={exp}")
            }
            DeepLink::Dm {
                user_id,
                msg: Some(msg),
            } => format!("{SCHEME}://dm/{user_id}?msg={msg}"),
            DeepLink::Dm { user_id, msg: None } => format!("{SCHEME}://dm/{user_id}"),
            DeepLink::Room { room_id } => format!("{SCHEME}://room/{room_id}"),
        }
    }

    pub fn parse(url: &str) -> Result<Self, DeepLinkError> {
        let url = url.trim();
        let rest = url
            .get(..6)
            .filter(|prefix| prefix.eq_ignore_ascii_case("app://"))
            .and_then(|_| url.get(6..))
            .ok_or(DeepLinkError::Scheme)?;
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        let mut segments = path.trim_matches('/').split('/');
        let kind = segments.next().unwrap_or("");
        let id = segments.next().ok_or(DeepLinkError::Path)?;
        if segments.next().is_some() {
            return Err(DeepLinkError::Path);
        }
        let param = |key: &str| -> Option<&str> {
            query
                .split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v)
        };
        let number = |name: &'static str, text: &str| -> Result<u64, DeepLinkError> {
            text.parse::<u64>().map_err(|_| DeepLinkError::Number(name))
        };
        match kind.to_ascii_lowercase().as_str() {
            "call" => Ok(DeepLink::Call {
                call_id: number("callId", id)?,
                from: number("from", param("from").ok_or(DeepLinkError::Missing("from"))?)?,
                exp: number("exp", param("exp").ok_or(DeepLinkError::Missing("exp"))?)?,
            }),
            "dm" => {
                let msg = match param("msg") {
                    Some(text) => Some(number("msg", text)?),
                    None => None,
                };
                Ok(DeepLink::Dm {
                    user_id: number("userId", id)?,
                    msg,
                })
            }
            "room" => Ok(DeepLink::Room {
                room_id: number("roomId", id)?,
            }),
            _ => Err(DeepLinkError::Path),
        }
    }
}

impl fmt::Display for DeepLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_url())
    }
}

impl FromStr for DeepLink {
    type Err = DeepLinkError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let links = [
            DeepLink::Call {
                call_id: 42,
                from: 7,
                exp: 1_700_000_000,
            },
            DeepLink::Dm {
                user_id: 7,
                msg: Some(99),
            },
            DeepLink::Dm {
                user_id: 7,
                msg: None,
            },
            DeepLink::Room { room_id: 3 },
        ];
        for link in links {
            assert_eq!(DeepLink::parse(&link.to_url()).unwrap(), link, "{link}");
        }
    }

    #[test]
    fn exact_forms() {
        assert_eq!(
            DeepLink::Call {
                call_id: 42,
                from: 7,
                exp: 1_700_000_000
            }
            .to_url(),
            "app://call/42?from=7&exp=1700000000"
        );
        assert_eq!(
            DeepLink::Dm {
                user_id: 7,
                msg: Some(99)
            }
            .to_url(),
            "app://dm/7?msg=99"
        );
        assert_eq!(DeepLink::Room { room_id: 3 }.to_url(), "app://room/3");
    }

    #[test]
    fn tolerant_parsing() {
        assert_eq!(
            DeepLink::parse(" APP://Room/3/ ").unwrap(),
            DeepLink::Room { room_id: 3 }
        );
        assert_eq!(
            DeepLink::parse("app://call/1?exp=5&from=2&extra=x").unwrap(),
            DeepLink::Call {
                call_id: 1,
                from: 2,
                exp: 5
            }
        );
    }

    #[test]
    fn errors() {
        assert_eq!(
            DeepLink::parse("https://x/room/3"),
            Err(DeepLinkError::Scheme)
        );
        assert_eq!(DeepLink::parse("app://nope/3"), Err(DeepLinkError::Path));
        assert_eq!(DeepLink::parse("app://room"), Err(DeepLinkError::Path));
        assert_eq!(DeepLink::parse("app://room/3/4"), Err(DeepLinkError::Path));
        assert_eq!(
            DeepLink::parse("app://call/1?from=2"),
            Err(DeepLinkError::Missing("exp"))
        );
        assert_eq!(
            DeepLink::parse("app://call/x?from=2&exp=1"),
            Err(DeepLinkError::Number("callId"))
        );
        assert_eq!(
            DeepLink::parse("app://dm/1?msg=abc"),
            Err(DeepLinkError::Number("msg"))
        );
    }
}
