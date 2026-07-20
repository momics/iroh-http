export interface TestModeDiscoveryInfo {
  directAddress: string | null;
  directAddresses: string[];
}

export interface TestModeAdvertisement {
  port: number;
  addressTxt?: string;
  usesBoundPort: boolean;
}

function socketPort(address: string): number | null {
  const separator = address.lastIndexOf(":");
  if (separator <= 0 || separator === address.length - 1) return null;

  const raw = address.slice(separator + 1);
  if (!/^\d{1,5}$/.test(raw)) return null;
  const port = Number(raw);
  return port >= 1 && port <= 65_535 ? port : null;
}

/**
 * Build the test-mode DNS-SD carrier from the endpoint's current discovery
 * snapshot. When a routable candidate exists its QUIC port is already bound by
 * `node.serve()`, which makes advertising it a regression check for #366.
 */
export function testModeAdvertisement(
  info: TestModeDiscoveryInfo | null,
): TestModeAdvertisement {
  const addresses = info?.directAddresses.length
    ? info.directAddresses
    : (info?.directAddress ? [info.directAddress] : []);
  const port = addresses.map(socketPort).find((candidate) =>
    candidate !== null
  );

  if (port === undefined) {
    return { port: 1, usesBoundPort: false };
  }

  return {
    port,
    addressTxt: addresses.join(","),
    usesBoundPort: true,
  };
}
