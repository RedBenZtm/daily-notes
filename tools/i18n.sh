#!/usr/bin/env bash
# Regenerate po/daily-notes.pot from the sources, merge it into each language,
# and compile the catalogues so `cargo run` picks them up.
#
#   tools/i18n.sh            # extract + merge + compile
#   tools/i18n.sh compile    # compile only (after editing a .po by hand)
set -euo pipefail
cd "$(dirname "$0")/.."

DOMAIN=daily-notes
MO_DIR=target/locale

if [ "${1:-all}" != "compile" ]; then
  # --keyword tells xgettext about our wrappers; ngettext:1,2 marks the
  # singular/plural pair. --add-comments picks up "Translators:" notes.
  # xtr, not xgettext: xgettext has no Rust mode, and parsing Rust as C makes
  # it pair up lifetime apostrophes (`&'a`) into bogus char literals, silently
  # swallowing every translatable string between them. Verified, not theoretical.
  #
  # xtr walks `mod` declarations from the crate root itself, so there is no
  # POTFILES list to keep in sync.
  command -v xtr >/dev/null || { echo "xtr missing: cargo install xtr" >&2; exit 1; }
  xtr --omit-header --package-name=$DOMAIN --package-version="$(
        sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)" \
      -o "po/$DOMAIN.pot" src/main.rs
  sed -i 's/charset=CHARSET/charset=UTF-8/' po/$DOMAIN.pot

  while read -r lang; do
    if [ -f "po/$lang.po" ]; then
      msgmerge --update --backup=none --quiet "po/$lang.po" "po/$DOMAIN.pot"
    else
      msginit --no-translator --locale="$lang" --input="po/$DOMAIN.pot" --output="po/$lang.po"
    fi
  done < po/LINGUAS
fi

fuzzy=0
while read -r lang; do
  mkdir -p "$MO_DIR/$lang/LC_MESSAGES"
  msgfmt --check --output-file="$MO_DIR/$lang/LC_MESSAGES/$DOMAIN.mo" "po/$lang.po"
  printf '%-4s %s\n' "$lang" "$(msgfmt --statistics --output-file=/dev/null "po/$lang.po" 2>&1)"
  n=$(grep -c '^#, fuzzy' "po/$lang.po" || true)
  if [ "$n" -gt 0 ]; then
    fuzzy=1
    echo "     ^ $n fuzzy entry/entries in po/$lang.po:" >&2
    grep -A2 '^#, fuzzy' "po/$lang.po" | grep '^msgid' | sed 's/^/       /' >&2
  fi
done < po/LINGUAS

# Fuzzy entries compile fine but gettext ignores them at runtime, so the app
# silently falls back to English. Fail loudly rather than ship that.
if [ "$fuzzy" -ne 0 ]; then
  echo >&2
  echo "error: fuzzy translations are NOT used at runtime -- review them and remove the '#, fuzzy' marker" >&2
  exit 1
fi
