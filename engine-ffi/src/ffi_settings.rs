//! Settings mirror. UniFFI records cannot hold BTreeMaps or foreign types, so the
//! engine's settings are copied field by field.

use engine::proto::peer::VideoCodec as EVideoCodec;
use engine::proto::DeviceId;
use engine::settings as es;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

impl From<EVideoCodec> for VideoCodec {
    fn from(c: EVideoCodec) -> Self {
        match c {
            EVideoCodec::H264 => VideoCodec::H264,
            EVideoCodec::Hevc => VideoCodec::Hevc,
            EVideoCodec::Av1 => VideoCodec::Av1,
        }
    }
}

impl From<VideoCodec> for EVideoCodec {
    fn from(c: VideoCodec) -> Self {
        match c {
            VideoCodec::H264 => EVideoCodec::H264,
            VideoCodec::Hevc => EVideoCodec::Hevc,
            VideoCodec::Av1 => EVideoCodec::Av1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CameraFacing {
    Front,
    Back,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct AudioSettings {
    pub bitrate_kbps: u32,
    pub redundancy: bool,
    pub jitter_override_ms: Option<u32>,
    pub voice_processing: bool,
    pub mic_device: Option<String>,
    pub speaker_device: Option<String>,
    /// Device id (hex) → volume 0.0..2.0.
    pub peer_volumes: HashMap<String, f32>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct VideoSettings {
    pub codec: VideoCodec,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    pub bitrate_kbps: u32,
    pub camera: CameraFacing,
    pub camera_id: Option<String>,
    pub mirror_self_view: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ScreenSettings {
    pub source: Option<String>,
    pub show_cursor: bool,
    pub codec: VideoCodec,
    pub width: u16,
    pub height: u16,
    pub fps: u16,
    pub bitrate_kbps: u32,
    pub system_audio: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct AdaptationSettings {
    pub lock_video_bitrate: bool,
    pub lock_fps: bool,
    pub lock_resolution: bool,
    pub lock_audio_bitrate: bool,
    pub av_sync: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FileSettings {
    pub auto_accept: bool,
    pub speed_cap_kbps: Option<u32>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct NotificationSettings {
    pub muted_users: Vec<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct Settings {
    pub audio: AudioSettings,
    pub video: VideoSettings,
    pub screen: ScreenSettings,
    pub adaptation: AdaptationSettings,
    pub files: FileSettings,
    pub notifications: NotificationSettings,
}

impl From<es::Settings> for Settings {
    fn from(s: es::Settings) -> Self {
        Settings {
            audio: AudioSettings {
                bitrate_kbps: s.audio.bitrate_kbps,
                redundancy: s.audio.redundancy,
                jitter_override_ms: s.audio.jitter_override_ms,
                voice_processing: s.audio.voice_processing,
                mic_device: s.audio.mic_device,
                speaker_device: s.audio.speaker_device,
                peer_volumes: s
                    .audio
                    .peer_volumes
                    .iter()
                    .map(|(k, v)| (k.to_hex(), *v))
                    .collect(),
            },
            video: VideoSettings {
                codec: s.video.codec.into(),
                width: s.video.width,
                height: s.video.height,
                fps: s.video.fps,
                bitrate_kbps: s.video.bitrate_kbps,
                camera: match s.video.camera {
                    es::CameraFacing::Front => CameraFacing::Front,
                    es::CameraFacing::Back => CameraFacing::Back,
                },
                camera_id: s.video.camera_id,
                mirror_self_view: s.video.mirror_self_view,
            },
            screen: ScreenSettings {
                source: s.screen.source,
                show_cursor: s.screen.show_cursor,
                codec: s.screen.codec.into(),
                width: s.screen.width,
                height: s.screen.height,
                fps: s.screen.fps,
                bitrate_kbps: s.screen.bitrate_kbps,
                system_audio: s.screen.system_audio,
            },
            adaptation: AdaptationSettings {
                lock_video_bitrate: s.adaptation.lock_video_bitrate,
                lock_fps: s.adaptation.lock_fps,
                lock_resolution: s.adaptation.lock_resolution,
                lock_audio_bitrate: s.adaptation.lock_audio_bitrate,
                av_sync: s.adaptation.av_sync,
            },
            files: FileSettings {
                auto_accept: s.files.auto_accept,
                speed_cap_kbps: s.files.speed_cap_kbps,
            },
            notifications: NotificationSettings {
                muted_users: s.notifications.muted_users.into_iter().collect(),
            },
        }
    }
}

impl From<Settings> for es::Settings {
    fn from(s: Settings) -> Self {
        es::Settings {
            audio: es::AudioSettings {
                bitrate_kbps: s.audio.bitrate_kbps,
                redundancy: s.audio.redundancy,
                jitter_override_ms: s.audio.jitter_override_ms,
                voice_processing: s.audio.voice_processing,
                mic_device: s.audio.mic_device,
                speaker_device: s.audio.speaker_device,
                peer_volumes: s
                    .audio
                    .peer_volumes
                    .iter()
                    .filter_map(|(k, v)| DeviceId::from_hex(k).ok().map(|id| (id, *v)))
                    .collect(),
            },
            video: es::VideoSettings {
                codec: s.video.codec.into(),
                width: s.video.width,
                height: s.video.height,
                fps: s.video.fps,
                bitrate_kbps: s.video.bitrate_kbps,
                camera: match s.video.camera {
                    CameraFacing::Front => es::CameraFacing::Front,
                    CameraFacing::Back => es::CameraFacing::Back,
                },
                camera_id: s.video.camera_id,
                mirror_self_view: s.video.mirror_self_view,
            },
            screen: es::ScreenSettings {
                source: s.screen.source,
                show_cursor: s.screen.show_cursor,
                codec: s.screen.codec.into(),
                width: s.screen.width,
                height: s.screen.height,
                fps: s.screen.fps,
                bitrate_kbps: s.screen.bitrate_kbps,
                system_audio: s.screen.system_audio,
            },
            adaptation: es::AdaptationSettings {
                lock_video_bitrate: s.adaptation.lock_video_bitrate,
                lock_fps: s.adaptation.lock_fps,
                lock_resolution: s.adaptation.lock_resolution,
                lock_audio_bitrate: s.adaptation.lock_audio_bitrate,
                av_sync: s.adaptation.av_sync,
            },
            files: es::FileSettings {
                auto_accept: s.files.auto_accept,
                speed_cap_kbps: s.files.speed_cap_kbps,
            },
            notifications: es::NotificationSettings {
                muted_users: s.notifications.muted_users.into_iter().collect(),
            },
        }
    }
}
