private enum ContractFailure: Error {
    case failed(String)
}

private func require(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    guard condition() else { throw ContractFailure.failed(message) }
}

@main
private struct PeerTxtPolicyContract {
    static func main() throws {
        try exactByteBoundary()
        try skipsNonFittingMemberButKeepsLaterFit()
        print("iOS peer TXT policy contract passed")
    }

    private static func exactByteBoundary() throws {
        // Seventeen valid socket literals produce exactly 247 UTF-8 bytes.
        // The next complete member must be omitted rather than truncated.
        let exactPrefix = (1 ... 17).map { index in
            let port = index <= 2 ? 20_000 : 2_000
            return "10.0.0.\(index):\(port)"
        }
        let expected = exactPrefix.joined(separator: ",")
        try require(expected.utf8.count == 247, "boundary fixture must be exactly 247 bytes")
        try require(
            "address=\(expected)".utf8.count == 255,
            "complete address TXT entry must be exactly 255 bytes"
        )

        let actual = stableFittingPeerAddressTxtValue(
            exactPrefix + ["10.0.0.18:2000"]
        )
        try require(actual == expected, "247-byte subset changed or overflow member leaked")
        try require(actual?.utf8.count == 247, "encoded address value must remain 247 bytes")

        let exactly247Bytes = String(repeating: "é", count: 123) + "a"
        let exactly248Bytes = String(repeating: "é", count: 124)
        try require(exactly247Bytes.utf8.count == 247, "UTF-8 247-byte fixture is invalid")
        try require(exactly248Bytes.utf8.count == 248, "UTF-8 248-byte fixture is invalid")
        try require(
            stableFittingPeerAddressTxtValue([exactly247Bytes]) == exactly247Bytes,
            "an exact 247-byte member must fit"
        )
        try require(
            stableFittingPeerAddressTxtValue([exactly248Bytes]) == nil,
            "a 248-byte member must be skipped whole"
        )
    }

    private static func skipsNonFittingMemberButKeepsLaterFit() throws {
        let base = (1 ... 16).map { index in
            let port = index <= 2 ? 20_000 : 2_000
            return "10.0.0.\(index):\(port)"
        }
        let nonFitting = "[2001:db8:1234:5678:9abc:def0:1234:5678]:4433"
        let laterFit = "8.8.8.8:2"

        guard let actual = stableFittingPeerAddressTxtValue(base + [nonFitting, laterFit]) else {
            throw ContractFailure.failed("fitting subset unexpectedly empty")
        }
        try require(!actual.contains(nonFitting), "non-fitting member must be skipped")
        try require(actual.hasSuffix(laterFit), "later shorter member must still be retained")
        try require(actual.utf8.count <= 247, "fitting subset exceeded the TXT value budget")
    }
}
