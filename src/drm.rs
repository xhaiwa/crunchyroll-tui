use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use base64::Engine;
use rsa::RsaPrivateKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use widevine::device::{DeviceType, SecurityLevel};
use widevine::{Cdm, Device, Key, KeyType, LicenseType, Pssh};

use crate::api::CrunchyrollClient;

const WIDEVINE_SYSTEM_ID: [u8; 16] = [
    0xed, 0xef, 0x8b, 0xa9, 0x79, 0xd6, 0x4a, 0xce, 0xa3, 0xc8, 0x27, 0xdc, 0xd5, 0x1d, 0x21, 0xed,
];

fn find_device_files() -> Result<(Option<PathBuf>, Option<PathBuf>, Option<PathBuf>)> {
    let mut wvd = None;
    let mut client_id = None;
    let mut private_key = None;
    for entry in fs::read_dir(".").context("scan current directory for Widevine device")? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "wvd") {
            wvd.get_or_insert(path);
        } else if path.file_name().is_some_and(|name| name == "client_id.bin") {
            client_id = Some(path);
        } else if path
            .file_name()
            .is_some_and(|name| name == "private_key.pem")
        {
            private_key = Some(path);
        }
    }
    Ok((wvd, client_id, private_key))
}

fn parse_private_key(bytes: &[u8]) -> Result<RsaPrivateKey> {
    if bytes.starts_with(b"-----") {
        let pem = std::str::from_utf8(bytes).context("private key PEM is not UTF-8")?;
        RsaPrivateKey::from_pkcs1_pem(pem)
            .or_else(|_| RsaPrivateKey::from_pkcs8_pem(pem))
            .context("parse RSA private key PEM")
    } else {
        RsaPrivateKey::from_pkcs1_der(bytes)
            .or_else(|_| RsaPrivateKey::from_pkcs8_der(bytes))
            .context("parse RSA private key DER")
    }
}

fn load_device() -> Result<Device> {
    let (wvd, client_id, private_key) = find_device_files()?;
    if let Some(path) = wvd {
        return Device::read_wvd(BufReader::new(
            File::open(&path).with_context(|| format!("open {}", path.display()))?,
        ))
        .with_context(|| format!("read Widevine device {}", path.display()));
    }
    if let (Some(client_id), Some(private_key)) = (client_id, private_key) {
        let client_id =
            fs::read(&client_id).with_context(|| format!("read {}", client_id.display()))?;
        let private_key_bytes =
            fs::read(&private_key).with_context(|| format!("read {}", private_key.display()))?;
        return Device::new(
            DeviceType::ANDROID,
            SecurityLevel::L3,
            parse_private_key(&private_key_bytes)?,
            &client_id,
        )
        .context("build Widevine device from raw files");
    }
    bail!(
        "no Widevine device found; provide either a .wvd file or both client_id.bin and private_key.pem in the current directory"
    )
}

/// Stamps the Widevine system ID onto a box the manifest already declared as Widevine,
/// because Crunchyroll ships some of them under a different system ID with a Widevine
/// payload inside. It rewrites the header only, so the caller has to have picked the box
/// out of a Widevine `ContentProtection`: over a PlayReady payload this would build a
/// challenge no license server can answer.
pub fn normalize_widevine_pssh(mut bytes: Vec<u8>) -> Result<Vec<u8>> {
    if bytes.len() < 32 {
        bail!("PSSH box is too short");
    }
    let size = u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if &bytes[4..8] != b"pssh" || size != bytes.len() {
        bail!("invalid PSSH box");
    }
    bytes[12..28].copy_from_slice(&WIDEVINE_SYSTEM_ID);
    Ok(bytes)
}

pub fn get_license_keys(
    client: &CrunchyrollClient,
    pssh_base64: &str,
    content_id: &str,
    video_token: &str,
) -> Result<Vec<Key>> {
    let pssh_bytes = base64::engine::general_purpose::STANDARD
        .decode(pssh_base64.trim())
        .context("decode manifest PSSH")?;
    let pssh =
        Pssh::from_bytes(&normalize_widevine_pssh(pssh_bytes)?).context("parse Widevine PSSH")?;
    let request = Cdm::new(load_device()?)
        .open()
        .get_license_request(pssh, LicenseType::AUTOMATIC)
        .context("create Widevine license request")?;
    let challenge = request.challenge().context("create Widevine challenge")?;
    let license = client.send_license_challenge(content_id, video_token, &challenge)?;
    let keys = request
        .get_keys(&license)
        .context("parse Widevine license")?;
    Ok(keys.of_type(KeyType::CONTENT).cloned().collect())
}

pub fn extract_default_kids(init_data: &[u8]) -> Vec<[u8; 16]> {
    let mut kids = Vec::new();
    for (type_index, marker) in init_data.windows(4).enumerate() {
        if marker != b"tenc" || type_index < 4 {
            continue;
        }
        let box_start = type_index - 4;
        let size = u32::from_be_bytes(
            init_data[box_start..type_index]
                .try_into()
                .expect("four-byte size"),
        ) as usize;
        let kid_start = type_index + 12;
        if size >= 32 && box_start + size <= init_data.len() && kid_start + 16 <= box_start + size {
            let kid: [u8; 16] = init_data[kid_start..kid_start + 16]
                .try_into()
                .expect("sixteen-byte KID");
            if !kids.contains(&kid) {
                kids.push(kid);
            }
        }
    }
    kids
}

fn matching_key<'a>(init_data: &[u8], keys: &'a [Key]) -> Result<&'a Key> {
    let kids = extract_default_kids(init_data);
    for kid in &kids {
        if let Some(key) = keys
            .iter()
            .find(|key| key.typ == KeyType::CONTENT && &key.kid == kid)
        {
            return Ok(key);
        }
    }
    if kids.is_empty() {
        bail!("MP4 initialization segment contains no encrypted track KID");
    }
    bail!(
        "no license key found for MP4 KID(s) {}",
        kids.iter().map(hex::encode).collect::<Vec<_>>().join(", ")
    )
}

/// Hex-encodes the license key that unlocks the track described by an MP4
/// initialization segment, in the form ffmpeg's `-decryption_key` expects.
pub fn content_key_hex(init_data: &[u8], keys: &[Key]) -> Result<String> {
    Ok(hex::encode(&matching_key(init_data, keys)?.key))
}

pub fn decrypt_mp4(init_data: &[u8], encrypted: &Path, output: &Path, keys: &[Key]) -> Result<()> {
    let key = content_key_hex(init_data, keys)?;
    let result = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-decryption_key",
        ])
        .arg(&key)
        .arg("-i")
        .arg(encrypted)
        .args(["-map", "0", "-c", "copy", "-f", "mp4"])
        .arg(output)
        .output()
        .context("run ffmpeg to decrypt MP4")?;
    if !result.status.success() {
        let _ = fs::remove_file(output);
        bail!(
            "ffmpeg MP4 decryption failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pssh(system_id: [u8; 16], payload: &[u8]) -> Vec<u8> {
        let size = 4 + 4 + 4 + 16 + 4 + payload.len();
        let mut result = Vec::with_capacity(size);
        result.extend_from_slice(&(size as u32).to_be_bytes());
        result.extend_from_slice(b"pssh");
        result.extend_from_slice(&[0; 4]);
        result.extend_from_slice(&system_id);
        result.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        result.extend_from_slice(payload);
        result
    }

    #[test]
    fn rewrites_pssh_system_id_only() {
        let playready = [
            0x9a, 0x04, 0xf0, 0x79, 0x98, 0x40, 0x42, 0x86, 0xab, 0x92, 0xe6, 0x5b, 0xe0, 0x88,
            0x5f, 0x95,
        ];
        let payload = [8, 1, 0x12, 0x10, 0xaa, 0xbb];
        let result = normalize_widevine_pssh(pssh(playready, &payload)).unwrap();
        assert_eq!(&result[12..28], &WIDEVINE_SYSTEM_ID);
        assert_eq!(&result[32..], &payload);
        let unchanged = pssh(WIDEVINE_SYSTEM_ID, &payload);
        assert_eq!(
            normalize_widevine_pssh(unchanged.clone()).unwrap(),
            unchanged
        );
    }

    #[test]
    fn extracts_tenc_kid() {
        let kid = [0xab; 16];
        let mut box_data = Vec::new();
        box_data.extend_from_slice(&32u32.to_be_bytes());
        box_data.extend_from_slice(b"tenc");
        box_data.extend_from_slice(&[0, 0, 0, 0]);
        box_data.extend_from_slice(&[0, 0, 1, 8]);
        box_data.extend_from_slice(&kid);
        assert_eq!(extract_default_kids(&box_data), vec![kid]);
    }
}
