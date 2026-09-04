//! Audio end to end: a sine pushed into one engine's microphone comes out of the
//! other engine's playback, through Opus, datagrams and the jitter buffer.
#![cfg(feature = "mock-server")]

mod common;

use common::*;
use engine::mock_server::{MockConfig, MockServer};
use engine::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FRAME: usize = 480;

/// Pushes a stereo sine in 10 ms chunks, in real time, until `stop`.
fn start_mic(
    engine: Engine,
    freq: f32,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut phase = 0f32;
        let mut ticker = tokio::time::interval(Duration::from_millis(10));
        let mut buf = vec![0f32; FRAME * 2];
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            ticker.tick().await;
            for i in 0..FRAME {
                let s = phase.sin() * 0.4;
                phase += 2.0 * std::f32::consts::PI * freq / 48_000.0;
                buf[2 * i] = s;
                buf[2 * i + 1] = s;
            }
            engine.push_mic(&buf, 2).unwrap();
        }
    })
}

/// Pulls 10 ms of stereo playback every 10 ms and keeps everything.
fn start_speaker(
    engine: Engine,
    sink: Arc<Mutex<Vec<f32>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(10));
        let mut buf = vec![0f32; FRAME * 2];
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            ticker.tick().await;
            engine.pull_playback(&mut buf, 2);
            sink.lock().unwrap().extend_from_slice(&buf);
        }
    })
}

fn mono(stereo: &[f32]) -> Vec<f32> {
    stereo.chunks(2).map(|c| (c[0] + c[1]) * 0.5).collect()
}

fn energy(samples: &[f32]) -> f32 {
    samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32
}

fn zero_crossings(samples: &[f32]) -> usize {
    samples
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sine_travels_between_engines_and_mute_silences_it() {
    let server = MockServer::start(MockConfig::default()).await.unwrap();
    let (a, b) = connected_pair(&server).await;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sink = Arc::new(Mutex::new(Vec::new()));
    let mic = start_mic(a.engine.clone(), 440.0, stop.clone());
    let speaker = start_speaker(b.engine.clone(), sink.clone(), stop.clone());

    tokio::time::sleep(Duration::from_millis(1500)).await;
    let captured = mono(&sink.lock().unwrap());
    // Skip the first second (jitter buffer fill, Opus warm-up), judge the last 400 ms.
    let tail = &captured[captured.len() - 48_000 * 400 / 1000..];
    let e = energy(tail);
    assert!(e > 0.01, "playback is silent: energy {e}");
    let crossings = zero_crossings(tail) as f32;
    let hz = crossings / 2.0 / 0.4;
    assert!(
        (380.0..=500.0).contains(&hz),
        "expected ~440 Hz, measured {hz}"
    );

    let stats_b = b.engine.stats();
    let peer = &stats_b.peers[0];
    assert!(
        peer.audio_in_kbps > 200.0,
        "audio in {} kbps",
        peer.audio_in_kbps
    );
    assert!(
        peer.jitter_target_ms >= 20.0 && peer.jitter_target_ms <= 200.0,
        "target {}",
        peer.jitter_target_ms
    );
    assert!(
        peer.audio_concealed < 30,
        "concealed {}",
        peer.audio_concealed
    );
    let stats_a = a.engine.stats();
    assert!(
        stats_a.peers[0].audio_out_kbps > 200.0,
        "audio out {}",
        stats_a.peers[0].audio_out_kbps
    );
    assert!(stats_a.mic_level > 0.3);

    // Mute: the far end hears silence, not concealment noise.
    a.engine.set_audio_muted(true);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let captured = mono(&sink.lock().unwrap());
    let tail = &captured[captured.len() - 48_000 * 200 / 1000..];
    assert!(
        energy(tail) < 1e-6,
        "still hearing audio after mute: {}",
        energy(tail)
    );

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = mic.await;
    let _ = speaker.await;
    a.engine.shutdown();
    b.engine.shutdown();
    server.shutdown().await;
}
