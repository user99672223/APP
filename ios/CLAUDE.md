# /ios — owner: iOS session

Only the iOS session edits this directory. Never build it on the Windows laptop:
GitHub Actions (/.github/workflows/ios.yml) builds the engine xcframework, generates
the Swift bindings, generates the Xcode project with XcodeGen from `project.yml`, and
produces an unsigned .ipa for sideloading.

SwiftUI app. Platform glue lives here: AVAudioEngine (voice processing, 5 ms IO buffer),
AVFoundation camera, VideoToolbox encode, AVSampleBufferDisplayLayer render, Keychain
device-bound storage key, `app://` deep links, background audio mode.
Everything else (networking, codecs' packetization, crypto, history, settings, stats)
is the Rust engine, reached through the generated `Generated/engine.swift`.

Only use APIs that compile on iOS 17+ with Swift 5 language mode. No third-party packages.
