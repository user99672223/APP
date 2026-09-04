import CoreMedia
import Foundation
import VideoToolbox

/// VideoToolbox hardware encoder (H.264 / HEVC): real-time, no B-frames, keyframe
/// every 2 s and on request, CBR-ish (SPEC §10). Output is Annex-B with the
/// parameter sets prepended to every keyframe, so a frame is self-describing.
final class VideoEncoder {
    struct Config: Equatable {
        var codec: VideoCodec
        var width: Int32
        var height: Int32
        var fps: Int32
        var bitrateKbps: Int32
    }

    private var session: VTCompressionSession?
    private(set) var config: Config?
    private var forceKeyframe = false
    private let queue = DispatchQueue(label: "video.encoder")
    /// (annexB bytes, keyframe, presentation time us, width, height, encode ms)
    var onFrame: ((Data, Bool, UInt64, UInt16, UInt16, Float) -> Void)?

    func configure(_ config: Config) {
        queue.sync {
            if self.config == config, session != nil { return }
            teardownLocked()
            self.config = config
            var created: VTCompressionSession?
            let codecType: CMVideoCodecType = config.codec == .h264 ? kCMVideoCodecType_H264 : kCMVideoCodecType_HEVC
            let status = VTCompressionSessionCreate(allocator: nil, width: config.width, height: config.height,
                                                    codecType: codecType, encoderSpecification: nil,
                                                    imageBufferAttributes: nil, compressedDataAllocator: nil,
                                                    outputCallback: nil, refcon: nil, compressionSessionOut: &created)
            guard status == noErr, let session = created else {
                NSLog("VideoEncoder: create failed \(status)")
                return
            }
            VTSessionSetProperty(session, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
            VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
            VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxKeyFrameInterval, value: (config.fps * 2) as CFNumber)
            VTSessionSetProperty(session, key: kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, value: 2 as CFNumber)
            VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: config.fps as CFNumber)
            VTSessionSetProperty(session, key: kVTCompressionPropertyKey_AverageBitRate, value: (config.bitrateKbps * 1000) as CFNumber)
            let bytesPerSecond = Int(config.bitrateKbps) * 1000 / 8
            VTSessionSetProperty(session, key: kVTCompressionPropertyKey_DataRateLimits, value: [bytesPerSecond, 1] as CFArray)
            let profile: CFString = config.codec == .h264 ? kVTProfileLevel_H264_High_AutoLevel : kVTProfileLevel_HEVC_Main_AutoLevel
            VTSessionSetProperty(session, key: kVTCompressionPropertyKey_ProfileLevel, value: profile)
            VTCompressionSessionPrepareToEncodeFrames(session)
            self.session = session
            forceKeyframe = true
        }
    }

    func requestKeyframe() {
        queue.async { self.forceKeyframe = true }
    }

    func stop() {
        queue.sync { teardownLocked() }
    }

    private func teardownLocked() {
        if let s = session {
            VTCompressionSessionInvalidate(s)
            session = nil
        }
        config = nil
    }

    /// Encode one captured frame. `timestampUs` is the engine media clock.
    func encode(_ pixelBuffer: CVPixelBuffer, timestampUs: UInt64) {
        queue.async {
            guard let session = self.session, let config = self.config else { return }
            let pts = CMTime(value: CMTimeValue(timestampUs), timescale: 1_000_000)
            let duration = CMTime(value: 1, timescale: config.fps)
            var props: [CFString: Any] = [:]
            if self.forceKeyframe {
                props[kVTEncodeFrameOptionKey_ForceKeyFrame] = kCFBooleanTrue
                self.forceKeyframe = false
            }
            let started = CFAbsoluteTimeGetCurrent()
            let codec = config.codec
            let onFrame = self.onFrame
            let status = VTCompressionSessionEncodeFrame(session, imageBuffer: pixelBuffer, presentationTimeStamp: pts,
                                                         duration: duration, frameProperties: props as CFDictionary,
                                                         infoFlagsOut: nil) { status, _, sampleBuffer in
                guard status == noErr, let sb = sampleBuffer, CMSampleBufferDataIsReady(sb),
                      let fmt = CMSampleBufferGetFormatDescription(sb) else { return }
                let encodeMs = Float((CFAbsoluteTimeGetCurrent() - started) * 1000)
                guard let data = AnnexB.fromSampleBuffer(sb, codec: codec) else { return }
                let keyframe = AnnexB.isKeyframe(sb)
                let dims = CMVideoFormatDescriptionGetDimensions(fmt)
                let ptsUs = CMSampleBufferGetPresentationTimeStamp(sb).convertScale(1_000_000, method: .default).value
                onFrame?(data, keyframe, UInt64(max(0, ptsUs)), UInt16(dims.width), UInt16(dims.height), encodeMs)
            }
            if status != noErr {
                NSLog("VideoEncoder: encode failed \(status)")
            }
        }
    }
}
