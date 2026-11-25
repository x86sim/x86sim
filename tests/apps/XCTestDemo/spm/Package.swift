// swift-tools-version: 6.0
// x86Sim: a Swift package whose tests build on Linux (x86sim test --spm, x86sim dev test --xctest): XCTest and Swift
// Testing tests, one of each failing on purpose.
import PackageDescription

let package = Package(
    name: "CalcKit",
    platforms: [.iOS(.v17), .tvOS(.v17), .watchOS(.v10), .visionOS(.v1)],
    products: [.library(name: "CalcKit", targets: ["CalcKit"])],
    targets: [.target(name: "CalcKit"), .testTarget(name: "CalcKitTests", dependencies: ["CalcKit"])]
)
