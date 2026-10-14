# Daily Notes

A small GNOME app for keeping a work log, one page per day. It sits in the top
bar as a tray icon, so jotting something down is a click or a keystroke away,
and it understands GitLab references (`#123`, `&7`, …) well enough to show what
they point to.

- **Main window**: browse and edit days, search all notes, see the day's
  GitLab activity next to what you wrote.
- **Quick Note**: a small popup for adding an entry to today without opening
  the main window.
- **Tray menu**: today's and the last workday's entries at a glance, plus
  Quick Note, Search, Settings and Quit.

The UI is available in English, German, Spanish and French and follows the
desktop's language and date-format settings.

## Requirements

- Rust toolchain with edition 2024 support (Rust 1.85 or newer)
- GTK 4.22, libadwaita 1.9 and GtkSourceView 5.16 development packages
  (GNOME 50). On Fedora:

  ```sh
  sudo dnf install gtk4-devel libadwaita-devel gtksourceview5-devel
  ```

- For the tray icon on GNOME: the
  [AppIndicator and KStatusNotifierItem Support](https://extensions.gnome.org/extension/615/appindicator-support/)
  extension. Without it the app still works, just without the icon.
- Optional, for GitLab integration: a running secret service (GNOME Keyring),
  which stores the access token.
- Optional, for translations: GNU gettext and `xtr` (`cargo install xtr`).

## Building

```sh
cargo build
tools/i18n.sh compile   # optional: compile translations into target/locale
```

A build run straight from `target/` picks up its translations from
`target/locale` by itself.

## Starting and stopping

### Start

```sh
cargo run
# or, after building:
target/debug/daily-notes
```

This opens the main window and puts the Daily Notes icon in the top bar.

Only one instance ever runs. Starting `daily-notes` again while it is already
running does not launch a second copy; it brings the running one's window to
the front instead.

To open Quick Note directly:

```sh
target/debug/daily-notes --quick-note
```

This starts the app if it is not running yet. If it is, the running instance
opens Quick Note. You can also double-click the tray icon, or bind it to a
keyboard shortcut (by default <kbd>Super</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd>):

```sh
tools/install-keybinding.sh                    # bind Super+Shift+N
tools/install-keybinding.sh '<Super><Alt>n'    # bind a different shortcut
tools/install-keybinding.sh --remove           # remove the binding
```

The shortcut runs `target/debug/daily-notes --quick-note` from this checkout
unless you pass a different command as the second argument.

### Stop

Closing the windows does **not** stop Daily Notes. It keeps running in the
tray so Quick Note stays one click away. To quit:

- choose **Quit** from the tray icon's menu, or
- press <kbd>Ctrl</kbd>+<kbd>C</kbd> in the terminal it was started from, or
- send it `SIGTERM`, e.g. `pkill -x daily-notes`.

All three save any pending edits before exiting.

If the tray icon is not available, the process also keeps running after the
last window closes. Run `daily-notes` again to get the window back, or stop it
with `pkill -x daily-notes`.

### Logging

Logs go to stderr. The default level is `info`; set `RUST_LOG` for more detail:

```sh
RUST_LOG=daily_notes=debug cargo run
```

## Configuration and data

Everything lives in `~/.daily_notes/`:

| File          | Contents                                   |
| ------------- | ------------------------------------------ |
| `notes.db`    | All notes (SQLite)                         |
| `config.toml` | GitLab instance, main project, main group  |

Set these up under **Settings** in the tray menu. The GitLab access token is
stored in the GNOME keyring, never in `config.toml`.

Environment variables:

| Variable                | Effect                                            |
| ----------------------- | ------------------------------------------------- |
| `DAILY_NOTES_LOCALEDIR` | Directory to load compiled translations from      |
| `DAILY_NOTES_ICON`      | Path or icon name to use for the tray icon        |
| `RUST_LOG`              | Log filter (default `daily_notes=info`)           |

## Development

```sh
cargo test
cargo test -- --ignored keyring_round_trip   # touches the real keyring
```

Translations live in `po/`. After changing user-visible strings:

```sh
tools/i18n.sh                                  # extract, merge, compile
tools/set-translation.py po/de.po 'Quit=Beenden'
tools/i18n.sh compile
```

`tools/i18n.sh` fails if any entry is still marked fuzzy, because gettext
ignores fuzzy entries at runtime and the UI would fall back to English.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
