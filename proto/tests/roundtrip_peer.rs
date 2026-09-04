//! Every peer-protocol message and the E2E envelope must survive encode → decode.

mod common;

use common::*;
use proto::e2e::*;
use proto::encode;
use proto::peer::*;

#[test]
fn stream_headers_and_audio() {
    rt(StreamHeader::Ctrl { version: 1 });
    rt(StreamHeader::Chat { version: 1 });
    rt(StreamHeader::File(FileStreamHeader {
        version: 1,
        file_id: 5,
        name: "a.bin".into(),
        size: 10,
        hash: [2; 32],
        offset: 4,
    }));
    rt(StreamHeader::Video(VideoFrameHeader {
        version: 1,
        family: MediaFamily::Screen,
        frame_no: 3,
        timestamp_us: 99,
        codec: VideoCodec::Av1,
        keyframe: true,
        width: 1920,
        height: 1080,
        length: 1234,
    }));
    rt(AudioPacket {
        version: 1,
        family: MediaFamily::Camera,
        seq: 65535,
        timestamp: u32::MAX,
        channels: 2,
        frame: vec![1; 600],
        prev_frame: vec![],
    });
}

#[test]
fn ctrl_messages() {
    let ctrl = vec![
        CtrlMsg::Hello {
            app_version: "0.1.0".into(),
            user_id: 1,
            decode_caps: vec![VideoCodec::H264, VideoCodec::Hevc],
            audio_muted: false,
            video_on: true,
        },
        CtrlMsg::KeyframeRequest {
            family: MediaFamily::Camera,
        },
        CtrlMsg::MuteState {
            audio_muted: true,
            video_on: false,
        },
        CtrlMsg::CodecAnnounce(CodecAnnounce {
            family: MediaFamily::Camera,
            codec: VideoCodec::Hevc,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 4000,
        }),
        CtrlMsg::DecodeCapability {
            codecs: vec![VideoCodec::Hevc],
        },
        CtrlMsg::BitrateHint {
            family: MediaFamily::Screen,
            kbps: 6000,
        },
        CtrlMsg::Report(ReceiverReport {
            rtt_ms: 20,
            audio_loss_permille: 3,
            video_delay_ms: 15,
            video_dropped: 1,
            video_resets: 0,
        }),
        CtrlMsg::ScreenShare {
            active: true,
            with_audio: true,
        },
        CtrlMsg::FileOffer(FileOffer {
            file_id: 5,
            name: "a.bin".into(),
            size: 10,
            hash: [2; 32],
        }),
        CtrlMsg::FileAccept {
            file_id: 5,
            offset: 0,
        },
        CtrlMsg::FileReject { file_id: 5 },
        CtrlMsg::FileCancel { file_id: 5 },
        CtrlMsg::FileProgress {
            file_id: 5,
            received: 8,
        },
        CtrlMsg::FileDone {
            file_id: 5,
            ok: true,
        },
        CtrlMsg::HangUp,
        CtrlMsg::Ping { sent_us: 1 },
        CtrlMsg::Pong { sent_us: 1 },
    ];
    for msg in ctrl {
        rt(CtrlFrame::new(msg));
    }
}

#[test]
fn chat_messages() {
    rt(ChatFrame::new(ChatMsg::Message(envelope())));
    rt(ChatFrame::new(ChatMsg::Delivered { msg_id: 77 }));
    rt(MessageBody {
        version: E2E_VERSION,
        text: "hi".into(),
    });
}

#[test]
fn video_header_then_payload() {
    let header = VideoFrameHeader {
        version: 1,
        family: MediaFamily::Camera,
        frame_no: 1,
        timestamp_us: 2,
        codec: VideoCodec::Hevc,
        keyframe: false,
        width: 640,
        height: 480,
        length: 3,
    };
    let mut bytes = encode(&StreamHeader::Video(header.clone())).unwrap();
    bytes.extend_from_slice(&[9, 9, 9]);
    let (parsed, rest): (StreamHeader, &[u8]) = proto::decode_prefix(&bytes).unwrap();
    assert_eq!(parsed, StreamHeader::Video(header));
    assert_eq!(rest, &[9, 9, 9]);
}

#[test]
fn envelope_helpers() {
    let env = envelope();
    let single = env.for_device(&device(3)).unwrap();
    assert_eq!(single.keys.len(), 1);
    assert_eq!(single.keys[0].recipient_device, device(3));
    assert_eq!(single.signed_bytes().unwrap(), env.signed_bytes().unwrap());
    assert!(env.for_device(&device(8)).is_none());
    assert_ne!(
        EncryptedMessage::wrap_aad(77, &device(1), &device(2)),
        EncryptedMessage::wrap_aad(77, &device(2), &device(1))
    );
}
