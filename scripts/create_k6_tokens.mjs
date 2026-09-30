import { createPrivateKey, createPublicKey, randomBytes, sign } from "node:crypto";

const baseUrl = process.env.BASE_URL || "http://127.0.0.1:8081";
const tokenCount = Number(process.env.K6_TOKEN_COUNT || 30);
const base32Alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

function base32(bytes) {
  let value = 0;
  let bits = 0;
  let result = "";
  for (const byte of bytes) {
    value = (value << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      result += base32Alphabet[(value >>> (bits - 5)) & 31];
      bits -= 5;
    }
  }
  if (bits) result += base32Alphabet[(value << (5 - bits)) & 31];
  return result;
}

function crc16xmodem(bytes) {
  let crc = 0;
  for (const byte of bytes) {
    crc ^= byte << 8;
    for (let bit = 0; bit < 8; bit++) {
      crc = crc & 0x8000 ? ((crc << 1) ^ 0x1021) & 0xffff : (crc << 1) & 0xffff;
    }
  }
  return crc;
}

function wallet(seed) {
  const privateKey = createPrivateKey({
    key: Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), seed]),
    format: "der",
    type: "pkcs8",
  });
  const publicKey = createPublicKey(privateKey).export({ format: "der", type: "spki" }).subarray(-32);
  const payload = Buffer.concat([Buffer.from([48]), publicKey]);
  const crc = crc16xmodem(payload);
  return {
    privateKey,
    address: base32(Buffer.concat([payload, Buffer.from([crc & 255, crc >>> 8])])),
  };
}

async function post(path, body) {
  const response = await fetch(`${baseUrl}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error(`${path} failed with HTTP ${response.status}: ${await response.text()}`);
  return response.json();
}

const tokens = [];
for (let i = 0; i < tokenCount; i++) {
  const { privateKey, address } = wallet(randomBytes(32));
  const { message } = await post("/api/v1/auth/nonce", { wallet_address: address });
  const signature = sign(null, Buffer.from(message), privateKey).toString("base64");
  const { token } = await post("/api/v1/auth/verify", {
    wallet_address: address,
    message,
    signature,
  });
  tokens.push(token);
  await new Promise((resolve) => setTimeout(resolve, 450));
}
process.stdout.write(`${tokens.join(",")}\n`);
