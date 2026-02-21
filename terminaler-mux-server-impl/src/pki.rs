// STRIPPED: PKI/TLS certificate management removed (TLS support stripped in Phase 0).
// This will be re-evaluated if TLS multiplexer support is needed.
use std::path::PathBuf;

/// Stub PKI helper - TLS support has been stripped.
pub struct Pki {
    pki_dir: PathBuf,
}

impl Pki {
    pub fn init() -> anyhow::Result<Self> {
        let pki_dir = config::pki_dir()?;
        std::fs::create_dir_all(&pki_dir)?;
        Ok(Self { pki_dir })
    }

    pub fn generate_client_cert(&self) -> anyhow::Result<String> {
        anyhow::bail!("TLS support has been stripped from this build")
    }

    pub fn ca_pem_string(&self) -> anyhow::Result<String> {
        anyhow::bail!("TLS support has been stripped from this build")
    }

    pub fn ca_pem(&self) -> PathBuf {
        self.pki_dir.join("ca.pem")
    }

    pub fn server_pem(&self) -> PathBuf {
        self.pki_dir.join("server.pem")
    }
}
