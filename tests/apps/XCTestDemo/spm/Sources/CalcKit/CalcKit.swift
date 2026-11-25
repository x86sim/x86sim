public enum CalcKit {
    public static func add(_ a: Int, _ b: Int) -> Int { a + b }
    public static func mean(_ xs: [Double]) -> Double? { xs.isEmpty ? nil : xs.reduce(0, +) / Double(xs.count) }
}
