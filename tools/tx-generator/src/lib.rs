use std::path::Path;

use serde::Deserialize;
use shell_crypto::{DilithiumSigner, Signer};
use shell_primitives::Address;

pub struct LoadedDevAuthority {
    pub signer: DilithiumSigner,
    pub address: Address,
    pub pubkey: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct DevAuthorityKeyFile {
    public_key: String,
    secret_key: String,
}

pub fn load_dev_authority(path: &Path) -> Result<LoadedDevAuthority, String> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read funding key '{}': {e}", path.display()))?;
    let stored: DevAuthorityKeyFile = serde_json::from_str(&json)
        .map_err(|e| format!("failed to decode funding key '{}': {e}", path.display()))?;

    let pubkey = hex::decode(stored.public_key.trim_start_matches("0x"))
        .map_err(|e| format!("invalid funding public key in '{}': {e}", path.display()))?;
    let secret_key = hex::decode(stored.secret_key.trim_start_matches("0x"))
        .map_err(|e| format!("invalid funding secret key in '{}': {e}", path.display()))?;
    let signer = DilithiumSigner::from_bytes(&pubkey, &secret_key).map_err(|e| {
        format!(
            "failed to load funding signer from '{}': {e}",
            path.display()
        )
    })?;
    let address = Address::from_public_key(&pubkey, signer.sig_type().as_u8());

    Ok(LoadedDevAuthority {
        signer,
        address,
        pubkey,
    })
}
