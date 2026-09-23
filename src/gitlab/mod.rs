pub mod cache;
pub mod client;
pub mod refs;

/// Everything the background resolver needs for one batch.
///
/// Assembled on the GTK thread and sent across, because reading the token
/// means talking to the secret service — and the keyring crate drives that
/// with its own `block_on`, which panics outright on a thread that is already
/// driving async tasks ("Cannot start a runtime from within a runtime").
#[derive(Debug, Clone)]
pub struct ResolveBatch {
    pub refs: Vec<refs::Reference>,
    pub base: String,
    pub token: String,
    pub main_project: String,
    pub main_group: String,
}

/// One reference, resolved (or definitively not) — what the background
/// resolver hands back to the UI thread.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub key: String,
    pub kind: &'static str,
    pub resolved: Option<client::Resolved>,
}
