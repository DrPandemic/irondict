pub mod model;
pub mod stardict;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("StarDict error: {0}")]
    Stardict(#[source] Box<dyn std::error::Error + Send + Sync>),
}

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
