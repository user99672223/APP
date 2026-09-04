import SwiftUI
import UIKit

/// Every setting from SPEC §14, all manual, all persisted by the engine.
struct SettingsView: View {
    @EnvironmentObject var model: AppModel
    @State var deviceName = UIDevice.current.name

    func bind<T>(_ keyPath: WritableKeyPath<Settings, T>) -> Binding<T> {
        Binding(
            get: { model.settings![keyPath: keyPath] },
            set: { value in
                var s = model.settings!
                s[keyPath: keyPath] = value
                model.updateSettings(s)
            }
        )
    }

    func bindDouble(_ keyPath: WritableKeyPath<Settings, UInt32>) -> Binding<Double> {
        Binding(get: { Double(model.settings![keyPath: keyPath]) },
                set: { value in
                    var s = model.settings!
                    s[keyPath: keyPath] = UInt32(value)
                    model.updateSettings(s)
                })
    }

    var body: some View {
        NavigationStack {
            if model.settings == nil {
                ProgressView()
            } else {
                Form {
                    audioSection
                    videoSection
                    adaptationSection
                    filesSection
                    notificationsSection
                    accountSection
                    aboutSection
                }
                .navigationTitle("Settings")
            }
        }
    }

    var audioSection: some View {
        Section("Audio") {
            VStack(alignment: .leading) {
                Text("Bitrate ceiling: \(model.settings!.audio.bitrateKbps) kbps")
                Slider(value: bindDouble(\.audio.bitrateKbps), in: 6...510, step: 2)
            }
            Toggle("Redundancy (previous frame in every packet)", isOn: bind(\.audio.redundancy))
            Toggle("Voice processing (echo cancel, noise suppression, AGC)", isOn: bind(\.audio.voiceProcessing))
            Toggle("Fixed jitter buffer", isOn: Binding(
                get: { model.settings!.audio.jitterOverrideMs != nil },
                set: { on in
                    var s = model.settings!
                    s.audio.jitterOverrideMs = on ? 60 : nil
                    model.updateSettings(s)
                }))
            if let ms = model.settings!.audio.jitterOverrideMs {
                VStack(alignment: .leading) {
                    Text("Jitter buffer: \(ms) ms")
                    Slider(value: Binding(get: { Double(ms) }, set: { v in
                        var s = model.settings!
                        s.audio.jitterOverrideMs = UInt32(v)
                        model.updateSettings(s)
                    }), in: 10...500, step: 10)
                }
            }
        }
    }

    var videoSection: some View {
        Section {
            Picker("Codec", selection: bind(\.video.codec)) {
                Text("HEVC").tag(VideoCodec.hevc)
                Text("H.264").tag(VideoCodec.h264)
                Text("AV1 (falls back to HEVC on iOS)").tag(VideoCodec.av1)
            }
            Picker("Resolution", selection: Binding(
                get: { model.settings!.video.height },
                set: { h in
                    var s = model.settings!
                    s.video.height = h
                    s.video.width = h * 16 / 9
                    model.updateSettings(s)
                })) {
                Text("1080p").tag(UInt16(1080))
                Text("720p").tag(UInt16(720))
                Text("540p").tag(UInt16(540))
                Text("360p").tag(UInt16(360))
            }
            Picker("Frame rate", selection: bind(\.video.fps)) {
                Text("60").tag(UInt16(60))
                Text("30").tag(UInt16(30))
                Text("15").tag(UInt16(15))
            }
            VStack(alignment: .leading) {
                Text("Bitrate ceiling: \(model.settings!.video.bitrateKbps / 1000) Mbps")
                Slider(value: bindDouble(\.video.bitrateKbps), in: 500...50_000, step: 500)
            }
            Picker("Camera", selection: bind(\.video.camera)) {
                Text("Front").tag(CameraFacing.front)
                Text("Back").tag(CameraFacing.back)
            }
            Toggle("Mirror self-view", isOn: bind(\.video.mirrorSelfView))
        } header: {
            Text("Video")
        } footer: {
            Text("Screen sharing is sent from Windows only; this phone can watch shares.")
        }
    }
}
