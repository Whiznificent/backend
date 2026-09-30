export interface WalletSigner {
  walletAddress: string;
  signMessage(message: string): Promise<string>;
}

export interface WalletSession {
  token: string;
  wallet_address: string;
}

export interface SpotSnapshot {
  prices: Record<string, number>;
  vols: Record<string, number>;
}

export class ZenithApiError extends Error {
  constructor(
    message: string,
    public readonly code?: string,
    public readonly requestId?: string,
  ) {
    super(message);
    this.name = "ZenithApiError";
  }
}

async function readResponse<T>(response: Response): Promise<T> {
  const payload: unknown = await response.json();
  if (!response.ok) {
    const error = (payload as { error?: string | { code?: string; message?: string; request_id?: string } }).error;
    if (typeof error === "string") {
      throw new ZenithApiError(error);
    }
    throw new ZenithApiError(
      error?.message ?? `Zenith API request failed with HTTP ${response.status}`,
      error?.code,
      error?.request_id,
    );
  }
  return payload as T;
}

export async function signInWithWallet(
  baseUrl: string,
  wallet: WalletSigner,
): Promise<WalletSession> {
  const root = baseUrl.replace(/\/+$/, "");
  const nonce = await readResponse<{ nonce: string; message: string }>(
    await fetch(`${root}/api/v1/auth/nonce`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ wallet_address: wallet.walletAddress }),
    }),
  );
  const signature = await wallet.signMessage(nonce.message);
  return readResponse<WalletSession>(
    await fetch(`${root}/api/v1/auth/verify`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        wallet_address: wallet.walletAddress,
        message: nonce.message,
        signature,
      }),
    }),
  );
}

export function subscribeToSpot(
  baseUrl: string,
  onSnapshot: (snapshot: SpotSnapshot) => void,
  onError: (error: Error) => void,
): WebSocket {
  const endpoint = new URL("/api/v1/ws/spot", baseUrl);
  endpoint.protocol = endpoint.protocol === "https:" ? "wss:" : "ws:";
  const socket = new WebSocket(endpoint);
  socket.addEventListener("message", (event: MessageEvent<string>) => {
    try {
      onSnapshot(JSON.parse(event.data) as SpotSnapshot);
    } catch (error) {
      onError(error instanceof Error ? error : new Error(String(error)));
    }
  });
  socket.addEventListener("error", () => onError(new Error("Zenith spot WebSocket failed")));
  return socket;
}
