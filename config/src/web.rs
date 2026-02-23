use terminaler_dynamic::{FromDynamic, ToDynamic};

#[derive(Debug, Clone, FromDynamic, ToDynamic)]
pub struct WebAccessConfig {
    /// Whether the web access server is enabled
    #[dynamic(default)]
    pub enabled: bool,

    /// Address to bind the web server to (e.g. "127.0.0.1:9876" or "0.0.0.0:9876")
    #[dynamic(default = "default_bind_address")]
    pub bind_address: String,

    /// Authentication token. If not set, one will be auto-generated.
    #[dynamic(default)]
    pub token: Option<String>,
}

impl Default for WebAccessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: default_bind_address(),
            token: None,
        }
    }
}

fn default_bind_address() -> String {
    "127.0.0.1:9876".to_string()
}
