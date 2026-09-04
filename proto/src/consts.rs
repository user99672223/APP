//! Numbers and validators both sides must agree on.

/// Devices send a heartbeat this often on the control stream.
pub const HEARTBEAT_INTERVAL_SECS: u64 = 10;
/// A device is offline after this many consecutive missed heartbeats.
pub const HEARTBEAT_MISSES_BEFORE_OFFLINE: u64 = 2;
/// Silence after which the server marks a device offline, with slack for jitter.
pub const PRESENCE_TIMEOUT_SECS: u64 =
    HEARTBEAT_INTERVAL_SECS * HEARTBEAT_MISSES_BEFORE_OFFLINE + 5;
/// A ringing call becomes "missed" after this long.
pub const CALL_RING_TIMEOUT_SECS: u64 = 60;
/// The server pushes an ntfy notification when live delivery is not acked within this time.
pub const NOTIFY_ACK_TIMEOUT_MS: u64 = 2_000;
/// Stored messages expire after this long.
pub const PENDING_MESSAGE_TTL_SECS: u64 = 30 * 24 * 3600;
/// A room and its code expire after this much idle time.
pub const ROOM_IDLE_EXPIRY_SECS: u64 = 24 * 3600;

pub const ROOM_CODE_LEN: usize = 6;
pub const ROOM_CODE_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

pub const NTFY_TOPIC_LEN: usize = 32;
pub const NTFY_BASE_URL: &str = "https://ntfy.sh";
/// Notification texts are deliberately generic; content never reaches ntfy.
pub const NOTIFY_TITLE_CALL: &str = "Incoming call";
pub const NOTIFY_TITLE_MESSAGE: &str = "New message";
pub const NOTIFY_TITLE_ROOM: &str = "Room invite";

/// Largest frame on the control stream (stored message blobs included).
pub const MAX_CONTROL_FRAME_BYTES: usize = 1024 * 1024;
/// Largest frame on the peer `ctrl` and `chat` streams.
pub const MAX_PEER_FRAME_BYTES: usize = 256 * 1024;
/// Largest encoded video frame accepted on a video stream.
pub const MAX_VIDEO_FRAME_BYTES: u32 = 16 * 1024 * 1024;
pub const MAX_CHAT_TEXT_BYTES: usize = 16 * 1024;
pub const MAX_DEVICE_NAME_LEN: usize = 64;
pub const MAX_USERNAME_LEN: usize = 32;
pub const MIN_USERNAME_LEN: usize = 3;
pub const MAX_DISPLAY_NAME_LEN: usize = 64;
pub const MIN_PASSWORD_LEN: usize = 8;
pub const MAX_FILE_NAME_LEN: usize = 255;

pub const AUDIO_SAMPLE_RATE: u32 = 48_000;
pub const AUDIO_FRAME_MS: u32 = 10;
pub const AUDIO_FRAME_SAMPLES: usize = 480;
pub const DEFAULT_AUDIO_BITRATE_KBPS: u32 = 510;
pub const DEFAULT_JITTER_TARGET_MS: u32 = 20;
pub const DEFAULT_VIDEO_BITRATE_KBPS: u32 = 12_000;
pub const DEFAULT_VIDEO_WIDTH: u16 = 1920;
pub const DEFAULT_VIDEO_HEIGHT: u16 = 1080;
pub const DEFAULT_VIDEO_FPS: u16 = 60;
pub const KEYFRAME_INTERVAL_SECS: u32 = 2;

/// Upper-case a typed room code and check it. Whitespace and dashes are dropped so a
/// code read aloud as "AB3-9KZ" still works.
pub fn normalize_room_code(input: &str) -> Option<String> {
    let code: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let valid =
        code.len() == ROOM_CODE_LEN && code.bytes().all(|b| ROOM_CODE_ALPHABET.contains(&b));
    valid.then_some(code)
}

pub fn is_valid_ntfy_topic(topic: &str) -> bool {
    topic.len() == NTFY_TOPIC_LEN && topic.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Usernames: lower-case letters, digits and underscore, 3 to 32 characters.
pub fn is_valid_username(name: &str) -> bool {
    (MIN_USERNAME_LEN..=MAX_USERNAME_LEN).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_codes() {
        assert_eq!(normalize_room_code("ab3-9kz").as_deref(), Some("AB39KZ"));
        assert_eq!(normalize_room_code(" AB3 9KZ ").as_deref(), Some("AB39KZ"));
        assert_eq!(normalize_room_code("AB39K"), None);
        assert_eq!(normalize_room_code("AB39K!"), None);
    }

    #[test]
    fn ntfy_topics() {
        assert!(is_valid_ntfy_topic(&"a1".repeat(16)));
        assert!(!is_valid_ntfy_topic("short"));
        assert!(!is_valid_ntfy_topic(&"a-".repeat(16)));
    }

    #[test]
    fn usernames() {
        assert!(is_valid_username("varsha_1"));
        assert!(!is_valid_username("va"));
        assert!(!is_valid_username("Varsha"));
    }
}
