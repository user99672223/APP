//! Opus (libopus via audiopus): 48 kHz, 10 ms frames, RESTRICTED_LOWDELAY, CBR.

use crate::error::{EngineError, Result};
use audiopus::coder::{Decoder, Encoder};
use audiopus::packet::Packet;
use audiopus::{Application, Bitrate, Channels, MutSignals, SampleRate};

/// Samples per channel in one 10 ms frame at 48 kHz.
pub const FRAME_SAMPLES: usize = proto::consts::AUDIO_FRAME_SAMPLES;
/// Largest Opus packet we ever produce (510 kbps × 10 ms is 638 bytes).
pub const MAX_PACKET_BYTES: usize = 1500;

fn codec_err(e: audiopus::Error) -> EngineError {
    EngineError::Codec(e.to_string())
}

fn channels_of(n: u8) -> Channels {
    if n == 2 {
        Channels::Stereo
    } else {
        Channels::Mono
    }
}

pub struct OpusEncoder {
    inner: Encoder,
    channels: u8,
    bitrate_kbps: u32,
}

impl OpusEncoder {
    pub fn new(channels: u8, bitrate_kbps: u32) -> Result<Self> {
        let mut inner = Encoder::new(
            SampleRate::Hz48000,
            channels_of(channels),
            Application::LowDelay,
        )
        .map_err(codec_err)?;
        inner
            .set_bitrate(Bitrate::BitsPerSecond((bitrate_kbps * 1000) as i32))
            .map_err(codec_err)?;
        // Constant bitrate keeps every packet the same size, which the datagram
        // budget and the redundancy copy rely on.
        inner.set_vbr(false).map_err(codec_err)?;
        inner.set_inband_fec(false).map_err(codec_err)?;
        inner.set_complexity(7).map_err(codec_err)?;
        Ok(Self {
            inner,
            channels: if channels == 2 { 2 } else { 1 },
            bitrate_kbps,
        })
    }

    pub fn channels(&self) -> u8 {
        self.channels
    }

    pub fn bitrate_kbps(&self) -> u32 {
        self.bitrate_kbps
    }

    pub fn set_bitrate(&mut self, kbps: u32) -> Result<()> {
        let kbps = kbps.clamp(6, 510);
        if kbps != self.bitrate_kbps {
            self.inner
                .set_bitrate(Bitrate::BitsPerSecond((kbps * 1000) as i32))
                .map_err(codec_err)?;
            self.bitrate_kbps = kbps;
        }
        Ok(())
    }

    /// `pcm`: 480 interleaved frames of `channels()` channels. `out` receives one packet.
    pub fn encode(&self, pcm: &[f32], out: &mut Vec<u8>) -> Result<()> {
        if pcm.len() != FRAME_SAMPLES * self.channels as usize {
            return Err(EngineError::Codec(format!(
                "expected {} samples, got {}",
                FRAME_SAMPLES * self.channels as usize,
                pcm.len()
            )));
        }
        out.clear();
        out.resize(MAX_PACKET_BYTES, 0);
        let n = self.inner.encode_float(pcm, out).map_err(codec_err)?;
        out.truncate(n);
        Ok(())
    }
}

pub struct OpusDecoder {
    inner: Decoder,
    channels: u8,
}

impl OpusDecoder {
    pub fn new(channels: u8) -> Result<Self> {
        let inner = Decoder::new(SampleRate::Hz48000, channels_of(channels)).map_err(codec_err)?;
        Ok(Self {
            inner,
            channels: if channels == 2 { 2 } else { 1 },
        })
    }

    pub fn channels(&self) -> u8 {
        self.channels
    }

    /// Decodes one packet into `out` (480 interleaved frames), or conceals a lost
    /// frame when `packet` is `None`/empty. Returns samples per channel produced.
    pub fn decode(&mut self, packet: Option<&[u8]>, out: &mut [f32]) -> Result<usize> {
        let packet = match packet {
            Some(p) if !p.is_empty() => Some(Packet::try_from(p).map_err(codec_err)?),
            _ => None,
        };
        let signals = MutSignals::try_from(out).map_err(codec_err)?;
        self.inner
            .decode_float(packet, signals, false)
            .map_err(codec_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(channels: usize, freq: f32, phase: &mut f32) -> Vec<f32> {
        let mut v = Vec::with_capacity(FRAME_SAMPLES * channels);
        for _ in 0..FRAME_SAMPLES {
            let s = (*phase).sin() * 0.5;
            *phase += 2.0 * std::f32::consts::PI * freq / 48_000.0;
            for _ in 0..channels {
                v.push(s);
            }
        }
        v
    }

    #[test]
    fn stereo_round_trip_and_plc() {
        let enc = OpusEncoder::new(2, 510).unwrap();
        let mut dec = OpusDecoder::new(2).unwrap();
        let mut phase = 0.0;
        let mut packet = Vec::new();
        let mut out = vec![0f32; FRAME_SAMPLES * 2];
        let mut sizes = Vec::new();
        for _ in 0..20 {
            enc.encode(&sine(2, 440.0, &mut phase), &mut packet)
                .unwrap();
            sizes.push(packet.len());
            assert_eq!(dec.decode(Some(&packet), &mut out).unwrap(), FRAME_SAMPLES);
        }
        // CBR at 510 kbps: about 638 bytes per 10 ms, never above the datagram budget.
        assert!(sizes.iter().all(|s| (500..=700).contains(s)), "{sizes:?}");
        let energy: f32 = out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32;
        assert!(energy > 0.01, "decoded audio is silent: {energy}");
        // Concealment produces a full frame too.
        assert_eq!(dec.decode(None, &mut out).unwrap(), FRAME_SAMPLES);
    }

    #[test]
    fn bitrate_changes_packet_size() {
        let mut enc = OpusEncoder::new(1, 510).unwrap();
        let mut phase = 0.0;
        let mut packet = Vec::new();
        enc.encode(&sine(1, 300.0, &mut phase), &mut packet)
            .unwrap();
        let big = packet.len();
        enc.set_bitrate(32).unwrap();
        for _ in 0..3 {
            enc.encode(&sine(1, 300.0, &mut phase), &mut packet)
                .unwrap();
        }
        assert!(packet.len() < big / 4, "{} vs {big}", packet.len());
        assert!(enc.encode(&[0.0; 10], &mut packet).is_err());
    }
}
