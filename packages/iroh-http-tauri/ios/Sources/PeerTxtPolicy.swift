/// Maximum UTF-8 byte length of the value in one `address=<value>` DNS-SD
/// TXT entry. The key and `=` consume eight of the entry's 255 bytes.
let peerAddressTxtValueMaxBytes = 247

/// Select a stable subset of complete address members for one DNS-SD TXT
/// value. Input order is retained; a member that does not fit is skipped so a
/// later shorter member can still use the remaining budget. Members are never
/// truncated.
func stableFittingPeerAddressTxtValue(_ candidates: [String]) -> String? {
    var fitted: [String] = []
    var usedBytes = 0

    for candidate in candidates where !candidate.isEmpty {
        let separatorBytes = fitted.isEmpty ? 0 : 1
        let additionalBytes = separatorBytes + candidate.utf8.count
        guard usedBytes + additionalBytes <= peerAddressTxtValueMaxBytes else {
            continue
        }
        fitted.append(candidate)
        usedBytes += additionalBytes
    }

    return fitted.isEmpty ? nil : fitted.joined(separator: ",")
}
