//! Video framing end to end: frames arrive in order, the codec falls back to
//! what every receiver can decode, keyframe requests reach the sender.
#![cfg(feature = "mock-server")]

mod common;

use bytes::Bytes;
use common::*;
use engine::events::EngineEvent;
use engine::mock_server::{MockConfig, MockServer};
use engine::proto::peer::{MediaFamily, VideoCodec};
use engine::video::EncodedFrame;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frames_flow_in_order_with_codec_fallback_and_keyframe_requests() {
    let server = MockServer::start(MockConfig::default()).await.unwrap();
    let (a, b) = connected_pair(&server).await;
    let (aid, bid) = (a.engine.device_id(), b.engine.device_id());

    // Alice wants AV1; Bob (test config) decodes only H.264/HEVC, so HEVC it is.
    let mut s = a.engine.settings();
    s.video.codec = VideoCodec::Av1;
    s.video.width = 1280;
    s.video.height = 720;
    s.video.fps = 30;
    s.video.bitrate_kbps = 4000;
    a.engine.update_settings(s).unwrap();
    a.engine.set_video_on(true);
    a.rec
        .wait_for(T, |e| {
            matches!(
                e,
                EngineEvent::EncoderConfig {
                    family: MediaFamily::Camera,
                    codec: VideoCodec::Hevc,
                    width: 1280,
                    ..
                }
            )
        })
        .await;
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::VideoFormat { device_id, codec: VideoCodec::Hevc, width: 1280, height: 720, .. } if *device_id == aid)).await;
    assert_eq!(
        a.engine.encoder_config(MediaFamily::Camera).unwrap().codec,
        VideoCodec::Hevc
    );

    // 60 synthetic frames at 30 fps, a keyframe every 30.
    for i in 0..60u32 {
        let keyframe = i % 30 == 0;
        let frame = EncodedFrame {
            family: MediaFamily::Camera,
            codec: VideoCodec::Hevc,
            keyframe,
            timestamp_us: a.engine.media_clock_us(),
            width: 1280,
            height: 720,
            frame_no: 0,
            data: Bytes::from(vec![i as u8; if keyframe { 60_000 } else { 15_000 }]),
        };
        a.engine.push_video_frame(frame).unwrap();
        tokio::time::sleep(Duration::from_millis(33)).await;
    }
    let deadline = std::time::Instant::now() + T;
    while b.rec.frames.lock().len() < 60 {
        assert!(
            std::time::Instant::now() < deadline,
            "only {} frames arrived",
            b.rec.frames.lock().len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let frames = b.rec.frames.lock().clone();
    assert!(frames.iter().all(|(from, _)| *from == aid));
    assert!(
        frames[0].1.keyframe,
        "first delivered frame must be a keyframe"
    );
    for w in frames.windows(2) {
        assert!(
            w[1].1.frame_no > w[0].1.frame_no,
            "out of order: {} after {}",
            w[1].1.frame_no,
            w[0].1.frame_no
        );
    }
    for (i, (_, f)) in frames.iter().enumerate() {
        assert_eq!(f.data[0], i as u8);
        assert_eq!(f.data.len(), if i % 30 == 0 { 60_000 } else { 15_000 });
        assert_eq!((f.width, f.height, f.codec), (1280, 720, VideoCodec::Hevc));
    }

    let sb = b.engine.stats();
    let peer = sb.peers.iter().find(|p| p.device_id == aid).unwrap();
    assert!(
        peer.video_in_fps > 5.0 && peer.video_in_fps < 40.0,
        "in fps {}",
        peer.video_in_fps
    );
    assert!(
        peer.video_in_kbps > 2000.0,
        "in kbps {}",
        peer.video_in_kbps
    );
    assert_eq!(peer.dropped_frames, 0);
    let sa = a.engine.stats();
    let peer_a = sa.peers.iter().find(|p| p.device_id == bid).unwrap();
    assert!(
        peer_a.video_out_fps > 5.0,
        "out fps {}",
        peer_a.video_out_fps
    );
    assert_eq!(peer_a.target_video_kbps, 4000);
    assert_eq!(peer_a.target_height, 720);

    // Bob's decoder asks for a keyframe: Alice's platform is told to produce one.
    b.engine.request_keyframe(aid, MediaFamily::Camera);
    a.rec
        .wait_for(T, |e| {
            matches!(
                e,
                EngineEvent::KeyframeRequested {
                    family: MediaFamily::Camera
                }
            )
        })
        .await;

    // Video off: pushing fails, and peers see it.
    a.engine.set_video_on(false);
    b.rec.wait_for(T, |e| matches!(e, EngineEvent::PeerMedia { device_id, video_on: false, .. } if *device_id == aid)).await;
    let frame = EncodedFrame {
        family: MediaFamily::Camera,
        codec: VideoCodec::Hevc,
        keyframe: true,
        timestamp_us: 0,
        width: 1280,
        height: 720,
        frame_no: 0,
        data: Bytes::from_static(b"x"),
    };
    assert!(a.engine.push_video_frame(frame).is_err());

    a.engine.shutdown();
    b.engine.shutdown();
    server.shutdown().await;
}
