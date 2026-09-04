import AVFoundation
import Foundation

/// Front/back camera capture (SPEC §10). Frames go to the encoder; the preview
/// layer is the mirrored self-view.
final class Camera: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate {
    private let session = AVCaptureSession()
    private let output = AVCaptureVideoDataOutput()
    private let queue = DispatchQueue(label: "camera.frames", qos: .userInteractive)
    private var input: AVCaptureDeviceInput?
    private(set) var facing: CameraFacing = .front
    let previewLayer: AVCaptureVideoPreviewLayer
    var onFrame: ((CVPixelBuffer) -> Void)?

    override init() {
        previewLayer = AVCaptureVideoPreviewLayer(session: session)
        previewLayer.videoGravity = .resizeAspectFill
        super.init()
        output.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange]
        output.alwaysDiscardsLateVideoFrames = true
        output.setSampleBufferDelegate(self, queue: queue)
    }

    var isRunning: Bool { session.isRunning }

    /// Picks the camera's best format at or below the requested size and rate.
    func start(facing: CameraFacing, width: Int, height: Int, fps: Int, mirrorPreview: Bool) throws {
        self.facing = facing
        session.beginConfiguration()
        defer { session.commitConfiguration() }
        if let old = input {
            session.removeInput(old)
            input = nil
        }
        let position: AVCaptureDevice.Position = facing == .front ? .front : .back
        guard let device = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: position) else {
            throw NSError(domain: "Camera", code: 1, userInfo: [NSLocalizedDescriptionKey: "no camera"])
        }
        let newInput = try AVCaptureDeviceInput(device: device)
        guard session.canAddInput(newInput) else {
            throw NSError(domain: "Camera", code: 2, userInfo: [NSLocalizedDescriptionKey: "cannot add camera input"])
        }
        session.addInput(newInput)
        input = newInput
        if !session.outputs.contains(output), session.canAddOutput(output) {
            session.addOutput(output)
        }
        session.sessionPreset = .inputPriority
        try device.lockForConfiguration()
        if let format = Camera.bestFormat(for: device, width: width, height: height, fps: fps) {
            device.activeFormat = format
            let maxRate = format.videoSupportedFrameRateRanges.map(\.maxFrameRate).max() ?? 30
            let rate = min(Double(fps), maxRate)
            device.activeVideoMinFrameDuration = CMTime(value: 1, timescale: CMTimeScale(rate))
            device.activeVideoMaxFrameDuration = CMTime(value: 1, timescale: CMTimeScale(rate))
        }
        device.unlockForConfiguration()
        if let connection = output.connection(with: .video) {
            if connection.isVideoRotationAngleSupported(90) { connection.videoRotationAngle = 90 }
            connection.isVideoMirrored = false
        }
        if let preview = previewLayer.connection {
            preview.automaticallyAdjustsVideoMirroring = false
            preview.isVideoMirrored = mirrorPreview && facing == .front
            if preview.isVideoRotationAngleSupported(90) { preview.videoRotationAngle = 90 }
        }
        if !session.isRunning {
            let s = session
            DispatchQueue.global(qos: .userInitiated).async { s.startRunning() }
        }
    }

    func stop() {
        guard session.isRunning else { return }
        let s = session
        DispatchQueue.global(qos: .userInitiated).async { s.stopRunning() }
    }

    private static func bestFormat(for device: AVCaptureDevice, width: Int, height: Int, fps: Int) -> AVCaptureDevice.Format? {
        // Largest format that fits inside the requested size and can do the rate, else the closest below.
        let candidates = device.formats.filter { f in
            let dims = CMVideoFormatDescriptionGetDimensions(f.formatDescription)
            let pixelFormat = CMFormatDescriptionGetMediaSubType(f.formatDescription)
            return pixelFormat == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
                && Int(dims.width) <= max(width, height) && Int(dims.height) <= min(width, height)
        }
        let withRate = candidates.filter { f in f.videoSupportedFrameRateRanges.contains { $0.maxFrameRate >= Double(fps) } }
        let pool = withRate.isEmpty ? candidates : withRate
        return pool.max { a, b in
            let da = CMVideoFormatDescriptionGetDimensions(a.formatDescription)
            let db = CMVideoFormatDescriptionGetDimensions(b.formatDescription)
            return Int(da.width) * Int(da.height) < Int(db.width) * Int(db.height)
        }
    }

    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        onFrame?(pixelBuffer)
    }
}
