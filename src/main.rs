mod app;
mod config;
mod gitlab;
mod i18n;
mod store;
mod tray;
mod ui;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "daily_notes=info".into()),
        )
        .with_target(false)
        .init();

    i18n::init();

    // Nothing with a side effect happens here: a remote invocation runs this
    // far too, and must not touch the database or the tray. See app::run.
    let code = app::run()?;
    std::process::exit(i32::from(code));
}
