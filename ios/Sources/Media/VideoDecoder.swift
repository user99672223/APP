import AVFoundation
import CoreMedia
import Foundation

/// One peer's video: compressed frames go straight into an
/// AVSampleBufferDisplayLayer, which decodes in hardware (SPEC §10). The format
/// description is rebuilt from the parameter sets that ride with each keyframe.
final class PeerVideoRenderer {
    let layer = AVSampleBufferDisplayLayer()
    private var format: CMVideoFormatDescription?
    private var codec: VideoCodec = .hevc
    private var waitingKeyframe = true
    private var lastKeyframeRequest = Date.distantPast
    var onNeedKeyframe: (() -> Void)?
    var onDecodeMs: ((Float) -> Void)?

    init() {
        layer.videoGravity = .resizeAspect
    }

    func reset() {
        format = nil
        waitingKeyframe = true
        layer.flush()
    }

    private func needKeyframe() {
        if Date().timeIntervalSince(lastKeyframeRequest) > 0.3 {
            lastKeyframeRequest = Date()
            onNeedKeyframe?()
        }
    }

    func render(_ frame: EncodedFrame) {
        let started = CFAbsoluteTimeGetCurrent()
        if codec != frame.codec {
            codec = frame.codec
            format = nil
            waitingKeyframe = true
        }
        let nals = AnnexB.nalUnits(Data(frame.data))
        let paramSets = nals.filter { AnnexB.isParameterSet($0, codec: codec) }
        let slices = nals.filter { !AnnexB.isParameterSet($0, codec: codec) }
        if !paramSets.isEmpty, let fmt = makeFormat(paramSets) {
            format = fmt
        }
        guard let fmt = format else {
            needKeyframe()
            return
        }
        if waitingKeyframe && !frame.keyframe {
            needKeyframe()
            return
        }
        waitingKeyframe = false
        guard !slices.isEmpty, let sample = makeSample(AnnexB.toLengthPrefixed(slices), format: fmt, timestampUs: frame.timestampUs) else { return }
        if layer.status == .failed {
            layer.flush()
            waitingKeyframe = true
            needKeyframe()
            return
        }
        layer.enqueue(sample)
        onDecodeMs?(Float((CFAbsoluteTimeGetCurrent() - started) * 1000))
    }

    private func makeFormat(_ sets: [Data]) -> CMVideoFormatDescription? {
        let buffers: [[UInt8]] = sets.map { [UInt8]($0) }
        let sizes = buffers.map { $0.count }
        var holders: [UnsafeMutablePointer<UInt8>] = []
        for b in buffers {
            let p = UnsafeMutablePointer<UInt8>.allocate(capacity: max(b.count, 1))
            p.initialize(from: b, count: b.count)
            holders.append(p)
        }
        defer { holders.forEach { $0.deallocate() } }
        let pointers: [UnsafePointer<UInt8>] = holders.map { UnsafePointer($0) }
        var fmt: CMVideoFormatDescription?
        let status: OSStatus = pointers.withUnsafeBufferPointer { pp in
            sizes.withUnsafeBufferPointer { sp in
                guard let pBase = pp.baseAddress, let sBase = sp.baseAddress else { return OSStatus(-1) }
                if codec == .h264 {
                    return CMVideoFormatDescriptionCreateFromH264ParameterSets(allocator: kCFAllocatorDefault, parameterSetCount: sets.count, parameterSetPointers: pBase, parameterSetSizes: sBase, nalUnitHeaderLength: 4, formatDescriptionOut: &fmt)
                }
                return CMVideoFormatDescriptionCreateFromHEVCParameterSets(allocator: kCFAllocatorDefault, parameterSetCount: sets.count, parameterSetPointers: pBase, parameterSetSizes: sBase, nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &fmt)
            }
        }
        return status == noErr ? fmt : nil
    }

    private func makeSample(_ data: Data, format: CMVideoFormatDescription, timestampUs: UInt64) -> CMSampleBuffer? {
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: data.count,
                                                 blockAllocator: nil, customBlockSource: nil, offsetToData: 0,
                                                 dataLength: data.count, flags: 0, blockBufferOut: &block) == noErr,
              let bb = block else { return nil }
        let copied: OSStatus = data.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return OSStatus(-1) }
            return CMBlockBufferReplaceDataBytes(with: base, blockBuffer: bb, offsetIntoDestination: 0, dataLength: data.count)
        }
        guard copied == noErr else { return nil }
        var timing = CMSampleTimingInfo(duration: .invalid,
                                        presentationTimeStamp: CMTime(value: CMTimeValue(timestampUs), timescale: 1_000_000),
                                        decodeTimeStamp: .invalid)
        var size = data.count
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: bb, formatDescription: format,
                                        sampleCount: 1, sampleTimingEntryCount: 1, sampleTimingArray: &timing,
                                        sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sample) == noErr,
              let sb = sample else { return nil }
        // The engine already paced this frame for A/V sync: show it now.
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: true) as? [CFMutableDictionary],
           let first = attachments.first {
            CFDictionarySetValue(first, Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                                 Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sb
    }
}
