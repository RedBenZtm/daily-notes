#!/usr/bin/env python3
"""Set translations in a .po file, clearing any '#, fuzzy' marker.

msgmerge guesses translations for changed strings and marks them fuzzy;
gettext then ignores them at runtime, so the app silently falls back to
English. Editing msgstr alone is not enough — the marker has to go too.

Handles gettext's multi-line form, where a long msgid is written as an empty
first line followed by continuation strings.

Usage: set-translation.py po/de.po 'msgid=msgstr' ...
                                   'ctx|msgid=msgstr' ...

The 'ctx|' prefix targets an entry with that msgctxt. Without it, only an
entry with no context matches — otherwise a short msgid like "group" would
set every context that happens to share it.
"""
import re
import sys


def unescape(text: str) -> str:
    """Undo the escapes gettext uses.

    Not `codecs.unicode_escape`: that decodes byte-wise as latin-1 and so
    mangles every non-ASCII character in the msgid (a "·" turns to mojibake
    and the lookup silently misses).
    """
    known = {"n": "\n", "t": "\t", "r": "\r", '"': '"', "\\": "\\"}
    out = []
    i = 0
    while i < len(text):
        if text[i] == "\\" and i + 1 < len(text):
            out.append(known.get(text[i + 1], text[i + 1]))
            i += 2
        else:
            out.append(text[i])
            i += 1
    return "".join(out)


def unquote(lines: list[str]) -> str:
    """Join gettext string continuation lines into one value."""
    out = []
    for line in lines:
        m = re.search(r'"((?:[^"\\]|\\.)*)"', line)
        if m:
            out.append(unescape(m.group(1)))
    return "".join(out)


def quote(value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")
    return f'"{escaped}"'


def main() -> int:
    path = sys.argv[1]
    wanted = {}
    for pair in sys.argv[2:]:
        key, _, msgstr = pair.partition("=")
        ctx, sep, msgid = key.partition("|")
        wanted[(ctx, msgid) if sep else (None, key)] = msgstr

    with open(path, encoding="utf-8") as fh:
        blocks = fh.read().split("\n\n")

    seen = set()
    for i, block in enumerate(blocks):
        lines = block.split("\n")
        # Collect the context, if the entry has one.
        ctx = None
        try:
            c_start = next(n for n, l in enumerate(lines) if l.startswith("msgctxt "))
            c_end = c_start + 1
            while c_end < len(lines) and lines[c_end].startswith('"'):
                c_end += 1
            ctx = unquote(lines[c_start:c_end])
        except StopIteration:
            pass
        # Collect the msgid, including continuation lines.
        try:
            start = next(n for n, l in enumerate(lines) if l.startswith("msgid "))
        except StopIteration:
            continue
        end = start + 1
        while end < len(lines) and lines[end].startswith('"'):
            end += 1
        msgid = unquote(lines[start:end])
        if (ctx, msgid) not in wanted:
            continue

        # Replace msgstr and everything continuing it.
        try:
            s_start = next(n for n, l in enumerate(lines) if l.startswith("msgstr "))
        except StopIteration:
            continue
        s_end = s_start + 1
        while s_end < len(lines) and lines[s_end].startswith('"'):
            s_end += 1

        new = lines[:s_start] + [f"msgstr {quote(wanted[(ctx, msgid)])}"] + lines[s_end:]
        # A fuzzy translation is ignored at runtime; drop the marker.
        new = [l for l in new if not l.startswith("#, fuzzy")]
        new = [l if l != "#," else l for l in new]
        blocks[i] = "\n".join(new)
        seen.add((ctx, msgid))

    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n\n".join(blocks))

    missing = sorted(
        (f"{c}|{m}" if c else m) for c, m in (set(wanted) - seen)
    )
    if missing:
        print(f"{path}: msgid not found: {missing}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
