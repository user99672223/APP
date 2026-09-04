import CoreMedia
import Foundation

/// Annex-B helpers shared by encoder and decoder: length-prefixed NAL units
/// (AVCC/HVCC) ↔ start-code delimited, with parameter sets on keyframes.
enum AnnexB {
    static let startCode: [UInt8] = [0, 0, 0, 1]

    static func isKeyframe(_ sb: CMSampleBuffer) -> Bool {
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[CFString: Any]],
              let first = attachments.first else { return true }
        if let notSync = first[kCMSampleAttachmentKey_NotSync] as? Bool { return !notSync }
        return true
    }

    /// Whole frame as Annex-B; keyframes get VPS/SPS/PPS in front.
    static func fromSampleBuffer(_ sb: CMSampleBuffer, codec: VideoCodec) -> Data? {
        guard let block = CMSampleBufferGetDataBuffer(sb), let fmt = CMSampleBufferGetFormatDescription(sb) else { return nil }
        var out = Data()
        if isKeyframe(sb) {
            let count = codec == .h264 ? 2 : 3
            for i in 0..<count {
                var ptr: UnsafePointer<UInt8>?
                var size = 0
                let status: OSStatus
                if codec == .h264 {
                    status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, parameterSetIndex: i, parameterSetPointerOut: &ptr, parameterSetSizeOut: &size, parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil)
                } else {
                    status = CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(fmt, parameterSetIndex: i, parameterSetPointerOut: &ptr, parameterSetSizeOut: &size, parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil)
                }
                if status == noErr, let p = ptr {
                    out.append(contentsOf: startCode)
                    out.append(p, count: size)
                }
            }
        }
        var length = 0
        var dataPointer: UnsafeMutablePointer<Int8>?
        guard CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &dataPointer) == noErr,
              let base = dataPointer else { return nil }
        var offset = 0
        while offset + 4 <= length {
            let nalLength = Int(UInt32(bigEndian: UnsafeRawPointer(base + offset).load(as: UInt32.self)))
            offset += 4
            guard nalLength > 0, offset + nalLength <= length else { break }
            out.append(contentsOf: startCode)
            out.append(UnsafeRawPointer(base + offset).assumingMemoryBound(to: UInt8.self), count: nalLength)
            offset += nalLength
        }
        return out
    }

    /// Split Annex-B into NAL units (without start codes).
    static func nalUnits(_ data: Data) -> [Data] {
        var units: [Data] = []
        let bytes = [UInt8](data)
        var i = 0
        var start: Int?
        while i + 2 < bytes.count {
            if bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 1 {
                if let s = start, i > s {
                    var end = i
                    if end > s && bytes[end - 1] == 0 { end -= 1 }
                    if end > s { units.append(Data(bytes[s..<end])) }
                }
                i += 3
                start = i
            } else {
                i += 1
            }
        }
        if let s = start, s < bytes.count { units.append(Data(bytes[s...])) }
        return units
    }

    /// NAL type and whether it is a parameter set (VPS/SPS/PPS).
    static func isParameterSet(_ nal: Data, codec: VideoCodec) -> Bool {
        guard let first = nal.first else { return false }
        if codec == .h264 {
            let type = Int(first & 0x1f)
            return type == 7 || type == 8
        }
        let type = Int((first >> 1) & 0x3f)
        return type == 32 || type == 33 || type == 34
    }

    /// Length-prefixed (AVCC/HVCC) form for CMSampleBuffer.
    static func toLengthPrefixed(_ nals: [Data]) -> Data {
        var out = Data()
        for nal in nals {
            var len = UInt32(nal.count).bigEndian
            out.append(Data(bytes: &len, count: 4))
            out.append(nal)
        }
        return out
    }
}
