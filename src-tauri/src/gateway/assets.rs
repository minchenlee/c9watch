//! Optional embedded SPA; a headless build without assets serves no page.
#[cfg(feature = "gateway-assets")]
#[derive(rust_embed::Embed)]
#[folder = "../build/"]
pub struct Assets;
#[cfg(not(feature = "gateway-assets"))]
pub struct Assets;
impl Assets {
    pub fn read(path: &str) -> Option<Vec<u8>> {
        #[cfg(feature = "gateway-assets")]
        {
            use rust_embed::RustEmbed;
            Self::get(path).map(|f| f.data.into_owned())
        }
        #[cfg(not(feature = "gateway-assets"))]
        {
            let _ = path;
            None
        }
    }
}
