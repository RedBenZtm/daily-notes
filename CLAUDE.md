# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Daily Notes is a Rust/GTK4/libadwaita tray app for Linux (GNOME, KDE Plasma): a
per-day work log with a Quick Note popup, full-text search, and GitLab
reference resolution (`#123`, `!45`, `&7`, `@user`, `group/project#123`).

## Commands

```sh
cargo build
cargo run                                     # main window + tray
cargo run -- --quick-note                     # Quick Note (forwards to a running instance)
RUST_LOG=daily_notes=debug cargo run

cargo test
cargo test store::blocks                      # one module
cargo test a_partial_config_still_parses      # one test, by name
cargo test -- --ignored keyring_round_trip    # touches the real secret service

tools/i18n.sh            # after changing user-visible strings: extract, merge, compile
tools/i18n.sh compile    # after hand-editing a .po
tools/set-translation.py po/de.po 'Quit=Beenden'
```

Only one instance runs at a time. If you start the app while another instance is already running, the new process just forwards to the running one. To test a fresh build, quit the running instance first (`pkill -x daily-notes`).

Your data lives in `~/.daily_notes/` (`notes.db`, `config.toml`). Store tests use `Store::open_in_memory()`, never the real database.

## Build floor

The crate targets GNOME 48 (GTK 4.18, libadwaita 1.7, GtkSourceView 5.16) so it builds on Debian 13. The `gnome_48` / `v1_7` / `v5_16` feature flags in `Cargo.toml` enforce this, so don't reach for newer APIs. The minimum Rust version is 1.92.

## Architecture

### Process and instance model (`src/app.rs`)

- `adw::Application` with `HANDLES_COMMAND_LINE`. Everything with a side effect (opening the DB, the tokio runtime, the tray's D-Bus name, workers) happens in `start_session`, which runs from `connect_startup`, **only on the primary instance**. `main` and `app::run` must stay side-effect free, because a remote invocation runs them too.
- `Session` owns all primary-instance state and wires the views together. When one view writes a day, it reloads the others and pushes a fresh tray snapshot.
- A `gio::ApplicationHoldGuard` keeps the process alive after the last window closes. Quit means dropping the guard.
- The `quick-note` app action is exported over D-Bus (`gapplication action de.ztm.DailyNotes quick-note`).

### Threads

- **GTK main thread** owns every widget and the `Store` (`Rc<RefCell<Store>>`, not `Send`).
- **tokio runtime** (2 workers) runs the tray (`ksni`) and all GitLab HTTP.
- The two sides talk only through `async_channel`. The tray module (`src/tray/`) is deliberately GTK-free. It receives a pre-rendered `TraySnapshot` (built on the GTK thread so dates follow `LC_TIME`) and sends `TrayCommand`s back.
- All `TrayCommand`s, including SIGTERM/SIGINT (which are turned into `TrayCommand::Quit`), are handled in one loop at the bottom of `start_session`, wrapped in `catch_unwind`. Quit flushes pending debounced saves before exiting.
- **The keyring must only be touched from the GTK thread.** The `keyring` crate's secret-service backend runs its own `block_on` and panics on a thread that already drives async tasks. `config::token()` is read on the GTK thread, cached in `Session`, and sent along inside each `ResolveBatch`. Keyring calls are also wrapped in `config::guarded` (catch_unwind).

### Storage (`src/store/`)

- `days.body` (markdown) is the single source of truth. There are no note files on disk.
- `blocks` is a derived projection (heading/bullet/para/code). Each save rebuilds it wholesale for that day, in the same transaction. `blocks_fts` (FTS5, `remove_diacritics 2`) is kept in sync by triggers. Block line numbers are **0-based** to match `gtk::TextBuffer`.
- Migrations in `schema.rs` are append-only and keyed on `user_version`. Never edit a migration that has shipped; add the next one.
- The `meta` key/value table holds UI state that should survive restarts (tray section, activity drawer open/closed).
- GitLab caches live in the same DB. In `gitlab_refs` (`src/gitlab/cache.rs`, 15-minute TTL), a network error must not be cached as "not found".

### GitLab (`src/gitlab/`)

`refs.rs` parses references, using byte offsets into the text. The editor paints synchronously from the cache. Misses and stale entries go to a background resolver task as a `ResolveBatch`, and the results come back to the GTK thread to be cached and repainted. Issue-list sync (for `#` completion) and per-day activity have their own worker tasks built the same way. Each batch builds its own `Client`, so Settings changes take effect without a restart.

### UI (`src/ui/`)

- `DayPane` is shared by `MainWindow` and the `QuickNote` last-workday drawer. It owns the buffer and its debounced save. Hosts only hear about a write after it happened.
- `Settings` (`settings.rs`) saves as you type; there's no OK button. The token is written to the keyring and never read back into the widget.
- `AdwActionRow`/`AdwEntryRow` titles and subtitles are parsed as Pango markup. For prose that may contain `&`, `<` or a server-supplied string, call `set_use_markup(false)` **before** setting the text.

### Config

`~/.daily_notes/config.toml` (`src/config.rs`) holds the GitLab instance, main project and main group. A malformed config falls back to the defaults rather than failing. The token never goes into the TOML.

## i18n

- The UI follows the desktop locale (`LC_MESSAGES` for text, `LC_TIME` for dates via `glib::DateTime`). There is no in-app language picker. The shipped languages are en (source), de, es and fr (`po/LINGUAS`).
- Use `crate::i18n::gettext`. Put placeholders in as `{}` and fill them with `.replace("{}", …)`, not `format!`. Add a `// Translators:` comment above any string with a placeholder or ambiguity.
- Extraction uses `xtr` (`cargo install xtr`), not xgettext. `tools/i18n.sh` fails on fuzzy entries because gettext ignores them at runtime. After changing strings, update all three `.po` files and make sure the script passes.
- A build run from `target/` finds its catalogs in `target/locale` automatically.

## Conventions

Commits follow Conventional Commits with a scope, in an imperative, user-facing voice. Examples: `fix(quicknote): focus the new-entry field on open`, `feat(tray): …`, `build: …`. Comments explain *why*, especially workarounds for platform behaviour that was actually observed. Keep that density when touching such code.
