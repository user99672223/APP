//! User settings. Every quality value is a ceiling: the adaptation controller only
//! goes below it (SPEC §13). Everything here is persisted in the encrypted store.

use proto::consts::*;
use proto::peer::VideoCodec;
use proto::{DeviceId, UserId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Settings {
    pub audio: AudioSettings,
    pub video: VideoSettings,
    pub screen: ScreenSettings,
    pub adaptation: AdaptationSettings,
    pub files: FileSettings,
    pub notifications: NotificationSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioSettings {
    pub bitrate_kbps: u32,
    pub redundancy: bool,
    /// `None` = adaptive jitter buffer, `Some(ms)` = fixed depth.
    pub jitter_override_ms: Option<u32>,
    /// Echo cancellation, noise suppression, AGC. Forces mono.
    pub voice_processing: bool,
    pub mic_device: Option<String>,
    pub speaker_device: Option<String>,
    /// 0.0 .. 2.0, default 1.0.
    pub peer_volumes: BTreeMap<DeviceId, f32>,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            bitrate_kbps: DEFAULT_AUDIO_BITRATE_KBPS,
            redundancy: true,
            jitter_override_ms: None,
            voice_processing: true,
            mic_device: None,
            speaker_device: None,
            peer_volumes: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraFacing {
    Front,
    Back,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoSettings {
    pub codec: VideoCodec,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    pub bitrate_kbps: u32,
    pub camera: CameraFacing,
    /// Windows: the camera device id; iOS uses `camera`.
    pub camera_id: Option<String>,
    pub mirror_self_view: bool,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            codec: VideoCodec::Hevc,
            width: DEFAULT_VIDEO_WIDTH,
            height: DEFAULT_VIDEO_HEIGHT,
            fps: DEFAULT_VIDEO_FPS,
            bitrate_kbps: DEFAULT_VIDEO_BITRATE_KBPS,
            camera: CameraFacing::Front,
            camera_id: None,
            mirror_self_view: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenSettings {
    /// Windows only: display or window id chosen by the user.
    pub source: Option<String>,
    pub show_cursor: bool,
    pub codec: VideoCodec,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    pub bitrate_kbps: u32,
    pub system_audio: bool,
}

impl Default for ScreenSettings {
    fn default() -> Self {
        Self {
            source: None,
            show_cursor: true,
            codec: VideoCodec::Hevc,
            width: DEFAULT_VIDEO_WIDTH,
            height: DEFAULT_VIDEO_HEIGHT,
            fps: DEFAULT_VIDEO_FPS,
            bitrate_kbps: DEFAULT_VIDEO_BITRATE_KBPS,
            system_audio: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdaptationSettings {
    pub lock_video_bitrate: bool,
    pub lock_fps: bool,
    pub lock_resolution: bool,
    pub lock_audio_bitrate: bool,
    /// Audio is the master clock; off = minimum latency.
    pub av_sync: bool,
}

impl Default for AdaptationSettings {
    fn default() -> Self {
        Self {
            lock_video_bitrate: false,
            lock_fps: false,
            lock_resolution: false,
            lock_audio_bitrate: false,
            av_sync: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FileSettings {
    pub auto_accept: bool,
    /// `None` = uncapped.
    pub speed_cap_kbps: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NotificationSettings {
    pub muted_users: BTreeSet<UserId>,
}

impl Settings {
    /// Reject values the pipelines cannot honour instead of silently clamping.
    pub fn validate(&self) -> Result<(), String> {
        let a = &self.audio;
        if !(6..=510).contains(&a.bitrate_kbps) {
            return Err("audio bitrate must be 6..=510 kbps".into());
        }
        if let Some(ms) = a.jitter_override_ms {
            if !(10..=1000).contains(&ms) {
                return Err("jitter buffer override must be 10..=1000 ms".into());
            }
        }
        for v in a.peer_volumes.values() {
            if !(0.0..=2.0).contains(v) {
                return Err("peer volume must be 0.0..=2.0".into());
            }
        }
        for (name, w, h, fps, kbps) in [
            (
                "video",
                self.video.width,
                self.video.height,
                self.video.fps,
                self.video.bitrate_kbps,
            ),
            (
                "screen",
                self.screen.width,
                self.screen.height,
                self.screen.fps,
                self.screen.bitrate_kbps,
            ),
        ] {
            if w < 16 || h < 16 {
                return Err(format!("{name} resolution too small"));
            }
            if !(1..=240).contains(&fps) {
                return Err(format!("{name} fps must be 1..=240"));
            }
            if !(100..=200_000).contains(&kbps) {
                return Err(format!("{name} bitrate must be 100..=200000 kbps"));
            }
        }
        if let Some(cap) = self.files.speed_cap_kbps {
            if cap == 0 {
                return Err("file speed cap must be > 0".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let s = Settings::default();
        assert_eq!(s.audio.bitrate_kbps, 510);
        assert!(s.audio.redundancy);
        assert!(s.audio.voice_processing);
        assert_eq!(s.video.codec, VideoCodec::Hevc);
        assert_eq!(
            (s.video.width, s.video.height, s.video.fps),
            (1920, 1080, 60)
        );
        assert_eq!(s.video.bitrate_kbps, 12_000);
        assert!(s.screen.system_audio);
        assert!(!s.files.auto_accept);
        assert!(s.adaptation.av_sync);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validation_rejects_bad_values() {
        let mut s = Settings::default();
        s.audio.bitrate_kbps = 600;
        assert!(s.validate().is_err());
        let mut s = Settings::default();
        s.video.fps = 0;
        assert!(s.validate().is_err());
    }

    #[test]
    fn round_trip() {
        let mut s = Settings::default();
        s.audio.peer_volumes.insert(DeviceId([1; 32]), 1.5);
        s.notifications.muted_users.insert(7);
        let bytes = proto::encode(&s).unwrap();
        assert_eq!(proto::decode::<Settings>(&bytes).unwrap(), s);
    }
}
