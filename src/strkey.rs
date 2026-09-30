//! Minimal Stellar "strkey" codec — supports G... (ed25519 public keys),
//! C... (Soroban contract IDs), and M... (Med25519 muxed accounts), with CRC16-XModem checksum validation.

use data_encoding::BASE32_NOPAD;

const ED25519_PUBLIC_KEY_VERSION_BYTE: u8 = 6 << 3; // encodes to the 'G' prefix (48)
const CONTRACT_VERSION_BYTE: u8 = 2 << 3;           // encodes to the 'C' prefix (16)
const MUXED_ACCOUNT_VERSION_BYTE: u8 = 12 << 3;     // encodes to the 'M' prefix (96)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressType {
    Ed25519PublicKey,
    Contract,
    MuxedAccount,
}

/// CRC16/XMODEM: poly 0x1021, init 0x0000, no reflection, no xor-out.
/// Stellar strkey uses this exact variant for its trailing checksum.
pub fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0x0000;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn encode_strkey(version_byte: u8, data: &[u8]) -> String {
    let mut payload = Vec::with_capacity(1 + data.len() + 2);
    payload.push(version_byte);
    payload.extend_from_slice(data);

    let checksum = crc16_xmodem(&payload);
    payload.extend_from_slice(&checksum.to_le_bytes());

    BASE32_NOPAD.encode(&payload)
}

fn decode_strkey(address: &str, expected_version_byte: u8, expected_payload_len: usize) -> Result<Vec<u8>, String> {
    let raw = BASE32_NOPAD
        .decode(address.as_bytes())
        .map_err(|_| "address is not valid base32".to_string())?;

    if raw.len() != expected_payload_len {
        return Err(format!(
            "expected a {expected_payload_len}-byte strkey payload, got {}",
            raw.len()
        ));
    }
    if raw[0] != expected_version_byte {
        return Err("address version byte mismatch".to_string());
    }

    let expected_checksum = u16::from_le_bytes([raw[raw.len() - 2], raw[raw.len() - 1]]);
    let actual_checksum = crc16_xmodem(&raw[0..raw.len() - 2]);
    if actual_checksum != expected_checksum {
        return Err("address checksum mismatch".to_string());
    }

    Ok(raw[1..raw.len() - 2].to_vec())
}

pub fn encode_stellar_public_key(pubkey: &[u8; 32]) -> String {
    encode_strkey(ED25519_PUBLIC_KEY_VERSION_BYTE, pubkey)
}

pub fn decode_stellar_public_key(address: &str) -> Result<[u8; 32], String> {
    let bytes = decode_strkey(address, ED25519_PUBLIC_KEY_VERSION_BYTE, 35)?;
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

pub fn encode_contract_id(contract_id: &[u8; 32]) -> String {
    encode_strkey(CONTRACT_VERSION_BYTE, contract_id)
}

pub fn decode_contract_id(address: &str) -> Result<[u8; 32], String> {
    let bytes = decode_strkey(address, CONTRACT_VERSION_BYTE, 35)?;
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

pub fn encode_muxed_account(account_id: u64, pubkey: &[u8; 32]) -> String {
    let mut data = Vec::with_capacity(40);
    data.extend_from_slice(&account_id.to_be_bytes());
    data.extend_from_slice(pubkey);
    encode_strkey(MUXED_ACCOUNT_VERSION_BYTE, &data)
}

pub fn decode_muxed_account(address: &str) -> Result<(u64, [u8; 32]), String> {
    let bytes = decode_strkey(address, MUXED_ACCOUNT_VERSION_BYTE, 43)?;
    let mut id_bytes = [0u8; 8];
    id_bytes.copy_from_slice(&bytes[0..8]);
    let account_id = u64::from_be_bytes(id_bytes);

    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&bytes[8..40]);
    Ok((account_id, pubkey))
}

pub fn validate_stellar_address(address: &str) -> Result<AddressType, String> {
    if address.starts_with('G') {
        decode_stellar_public_key(address).map(|_| AddressType::Ed25519PublicKey)
    } else if address.starts_with('C') {
        decode_contract_id(address).map(|_| AddressType::Contract)
    } else if address.starts_with('M') {
        decode_muxed_account(address).map(|_| AddressType::MuxedAccount)
    } else {
        Err("Unsupported address prefix. Must start with G, C, or M".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_ed25519_public_key() {
        let pubkey: [u8; 32] = std::array::from_fn(|i| i as u8);
        let address = encode_stellar_public_key(&pubkey);
        assert!(address.starts_with('G'));
        let decoded = decode_stellar_public_key(&address).unwrap();
        assert_eq!(decoded, pubkey);
        assert_eq!(validate_stellar_address(&address).unwrap(), AddressType::Ed25519PublicKey);
    }

    #[test]
    fn round_trips_contract_id() {
        let contract_id: [u8; 32] = [42u8; 32];
        let address = encode_contract_id(&contract_id);
        assert!(address.starts_with('C'));
        let decoded = decode_contract_id(&address).unwrap();
        assert_eq!(decoded, contract_id);
        assert_eq!(validate_stellar_address(&address).unwrap(), AddressType::Contract);
    }

    #[test]
    fn round_trips_muxed_account() {
        let pubkey: [u8; 32] = [99u8; 32];
        let account_id = 1234567890u64;
        let address = encode_muxed_account(account_id, &pubkey);
        assert!(address.starts_with('M'));
        let (dec_id, dec_pk) = decode_muxed_account(&address).unwrap();
        assert_eq!(dec_id, account_id);
        assert_eq!(dec_pk, pubkey);
        assert_eq!(validate_stellar_address(&address).unwrap(), AddressType::MuxedAccount);
    }

    #[test]
    fn rejects_corrupted_checksum() {
        let contract_id = [7u8; 32];
        let mut address = encode_contract_id(&contract_id);
        let last = address.pop().unwrap();
        address.push(if last == 'A' { 'B' } else { 'A' });
        assert!(decode_contract_id(&address).is_err());
    }
}
