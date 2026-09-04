import AVFoundation
import Foundation

/// Microphone in, mixed playback out, through AVAudioEngine with Apple's voice
/// processing (echo cancellation, noise suppression, AGC) when enabled. 48 kHz,
/// 5 ms preferred IO buffer (SPEC §9). Voice processing forces mono.
final class AudioIO {
    private let engine = AVAudioEngine()
    private var sourceNode: AVAudioSourceNode?
    private var running = false
    private let sampleRate: Double = 48_000
    private var pushMic: (([Float], UInt8) -> Void)?
    private var pullPlayback: ((UInt32, UInt8) -> [Float])?
    private(set) var voiceProcessing = true
    private var outputChannels: UInt8 = 2

    /// Starts capture and playback. `voiceProcessing` off means raw microphone
    /// (headphones recommended: no echo cancellation).
    func start(voiceProcessing: Bool,
               pushMic: @escaping ([Float], UInt8) -> Void,
               pullPlayback: @escaping (UInt32, UInt8) -> [Float]) throws {
        stop()
        self.voiceProcessing = voiceProcessing
        self.pushMic = pushMic
        self.pullPlayback = pullPlayback

        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.playAndRecord, mode: voiceProcessing ? .voiceChat : .default,
                                options: [.allowBluetooth, .defaultToSpeaker, .mixWithOthers])
        try session.setPreferredSampleRate(sampleRate)
        try session.setPreferredIOBufferDuration(0.005)
        try session.setActive(true)

        let input = engine.inputNode
        let output = engine.outputNode
        if voiceProcessing {
            try? input.setVoiceProcessingEnabled(true)
        } else {
            try? input.setVoiceProcessingEnabled(false)
        }

        // Capture: whatever the hardware gives, converted to 48 kHz float, 1 or 2 channels.
        let hwFormat = input.outputFormat(forBus: 0)
        let micChannels: UInt8 = voiceProcessing ? 1 : (hwFormat.channelCount >= 2 ? 2 : 1)
        guard let micFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: sampleRate,
                                            channels: AVAudioChannelCount(micChannels), interleaved: true),
              let converter = AVAudioConverter(from: hwFormat, to: micFormat) else {
            throw NSError(domain: "AudioIO", code: 1, userInfo: [NSLocalizedDescriptionKey: "unsupported mic format"])
        }
        input.installTap(onBus: 0, bufferSize: 480, format: hwFormat) { [weak self] buffer, _ in
            guard let self = self else { return }
            let ratio = self.sampleRate / hwFormat.sampleRate
            let capacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 16
            guard let out = AVAudioPCMBuffer(pcmFormat: micFormat, frameCapacity: capacity) else { return }
            var consumed = false
            var error: NSError?
            converter.convert(to: out, error: &error) { _, status in
                if consumed {
                    status.pointee = .noDataNow
                    return nil
                }
                consumed = true
                status.pointee = .haveData
                return buffer
            }
            guard error == nil, out.frameLength > 0, let data = out.floatChannelData else { return }
            let count = Int(out.frameLength) * Int(micChannels)
            let samples = Array(UnsafeBufferPointer(start: data[0], count: count))
            self.pushMic?(samples, micChannels)
        }

        // Playback: the engine mixes every peer; we just ask for frames.
        let outChannels: UInt8 = output.outputFormat(forBus: 0).channelCount >= 2 ? 2 : 1
        outputChannels = outChannels
        guard let playFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: sampleRate,
                                             channels: AVAudioChannelCount(outChannels), interleaved: false) else {
            throw NSError(domain: "AudioIO", code: 2, userInfo: [NSLocalizedDescriptionKey: "unsupported output format"])
        }
        let source = AVAudioSourceNode(format: playFormat) { [weak self] _, _, frameCount, audioBufferList -> OSStatus in
            guard let self = self, let pull = self.pullPlayback else { return noErr }
            let mixed = pull(frameCount, outChannels)
            let abl = UnsafeMutableAudioBufferListPointer(audioBufferList)
            for (ch, buf) in abl.enumerated() {
                guard let dst = buf.mData?.assumingMemoryBound(to: Float.self) else { continue }
                for i in 0..<Int(frameCount) {
                    let idx = i * Int(outChannels) + ch
                    dst[i] = idx < mixed.count ? mixed[idx] : 0
                }
            }
            return noErr
        }
        engine.attach(source)
        engine.connect(source, to: engine.mainMixerNode, format: playFormat)
        engine.connect(engine.mainMixerNode, to: output, format: nil)
        sourceNode = source
        engine.prepare()
        try engine.start()
        running = true
    }

    func stop() {
        guard running else { return }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        if let source = sourceNode {
            engine.detach(source)
            sourceNode = nil
        }
        running = false
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    var isRunning: Bool { running }
}
