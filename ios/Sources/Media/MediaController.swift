import AVFoundation
import Foundation

/// Owns the platform media glue for a call: microphone/speaker (AudioIO), camera
/// + encoder for our own video, one renderer per peer for theirs. Everything else
/// (packetization, jitter, mixing, sync, adaptation) is the engine.
@MainActor
final class MediaController: ObservableObject {
    @Published private(set) var callActive = false
    @Published private(set) var cameraOn = false
    @Published private(set) var renderers: [String: PeerVideoRenderer] = [:]

    private let engine: Engine
    private let audio = AudioIO()
    private let camera = Camera()
    private let encoder = VideoEncoder()
    private var encoderConfig: EncoderConfig?

    var previewLayer: AVCaptureVideoPreviewLayer { camera.previewLayer }

    init(engine: Engine) {
        self.engine = engine
        camera.onFrame = { [weak self] pixelBuffer in
            guard let self = self else { return }
            // Called on the capture queue: stamp with the engine clock and encode.
            self.encoder.encode(pixelBuffer, timestampUs: self.engine.mediaClockUs())
        }
        encoder.onFrame = { [weak self] data, keyframe, ts, width, height, encodeMs in
            guard let self = self, let cfg = self.encoderConfig else { return }
            let frame = EncodedFrame(family: .camera, codec: cfg.codec, keyframe: keyframe, timestampUs: ts,
                                     width: width, height: height, frameNo: 0, data: data)
            do {
                try self.engine.pushVideoFrame(frame: frame)
            } catch {
                NSLog("pushVideoFrame: \(error)")
            }
            self.engine.reportEncodeMs(family: .camera, ms: encodeMs)
        }
    }

    // MARK: audio

    /// Start mic + speaker for a call. Voice processing per settings (SPEC §9).
    func startAudio(voiceProcessing: Bool) {
        let engine = self.engine
        do {
            try audio.start(voiceProcessing: voiceProcessing,
                            pushMic: { samples, channels in
                                try? engine.pushMic(samples: samples, channels: channels)
                            },
                            pullPlayback: { frames, channels in
                                engine.pullPlayback(frames: frames, channels: channels)
                            })
            callActive = true
        } catch {
            NSLog("audio start failed: \(error)")
        }
    }

    func stopAll() {
        audio.stop()
        stopCamera()
        callActive = false
        renderers.removeAll()
    }

    // MARK: our video

    /// Camera on: the engine announces the codec and tells us the encoder config
    /// through `applyEncoderConfig`.
    func startCamera(facing: CameraFacing, mirror: Bool) {
        engine.setVideoOn(on: true)
        if let cfg = engine.encoderConfig(family: .camera) {
            applyEncoderConfig(cfg, facing: facing, mirror: mirror)
        }
        cameraOn = true
    }

    func stopCamera() {
        guard cameraOn else { return }
        camera.stop()
        encoder.stop()
        engine.setVideoOn(on: false)
        cameraOn = false
    }

    /// From the engine's EncoderConfig event (ceiling ∧ adaptation ∧ codec fallback).
    func applyEncoderConfig(_ cfg: EncoderConfig, facing: CameraFacing, mirror: Bool) {
        guard cfg.family == .camera else { return }
        encoderConfig = cfg
        let width = Int32(max(cfg.width, cfg.height))
        let height = Int32(min(cfg.width, cfg.height))
        encoder.configure(VideoEncoder.Config(codec: cfg.codec == .av1 ? .hevc : cfg.codec,
                                              width: width, height: height,
                                              fps: Int32(cfg.fps), bitrateKbps: Int32(cfg.bitrateKbps)))
        do {
            try camera.start(facing: facing, width: Int(width), height: Int(height), fps: Int(cfg.fps), mirrorPreview: mirror)
        } catch {
            NSLog("camera start failed: \(error)")
        }
    }

    func switchCamera(to facing: CameraFacing, mirror: Bool) {
        guard cameraOn, let cfg = encoderConfig else { return }
        applyEncoderConfig(cfg, facing: facing, mirror: mirror)
        encoder.requestKeyframe()
    }

    func produceKeyframe() {
        encoder.requestKeyframe()
    }

    // MARK: peers' video

    func renderer(for deviceId: String) -> PeerVideoRenderer {
        if let r = renderers[deviceId] { return r }
        let r = PeerVideoRenderer()
        let engine = self.engine
        r.onNeedKeyframe = { try? engine.requestKeyframe(deviceId: deviceId, family: .camera) }
        r.onDecodeMs = { ms in try? engine.reportDecodeMs(deviceId: deviceId, family: .camera, ms: ms) }
        renderers[deviceId] = r
        return r
    }

    /// Engine callback (any thread): hand the frame to that peer's renderer.
    nonisolated func receive(from deviceId: String, frame: EncodedFrame) {
        Task { @MainActor in
            self.renderer(for: deviceId).render(frame)
        }
    }

    func peerLeft(_ deviceId: String) {
        renderers.removeValue(forKey: deviceId)
    }
}
