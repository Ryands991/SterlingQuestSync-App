#!/usr/bin/env python3
"""Sterling code style for the Rust code, part 2 (run after `cargo fmt`).

    python3 tools/style.py            rewrite every .rs file in place
    python3 tools/style.py --check    only report files that would change (exit 1 if any)

What it does, outside strings and comments only (formatting, never code):
  - An opening brace, bracket or parenthesis is never the last thing on a line: it moves to its own
    line at the same indent ("match x {" becomes "match x" and "{").
  - "} else {" becomes three lines: "}", "else", "{".
  - A blank line after a closing brace, unless the next line closes something too or carries on the
    same expression (else, a method call, ?).
  - Grouping: statements next to each other that start differently (let, if, return, match, loops,
    everything else) get a blank line between them. A comment or attribute stays with the line below it.

Run it twice and nothing changes the second time. Proof that it only moves whitespace: the token
stream of every file is the same before and after (see the format commit's message).
"""

import pathlib
import re
import sys

OPENERS = "{[("
CLOSERS = "}])"


def scan(text):
    """Marks every character that is code (True) or inside a string, char or comment (False)."""
    code = [True] * len(text)
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        two = text[i:i + 2]
        if two == "//":
            j = text.find("\n", i)
            j = n if j < 0 else j
            for k in range(i, j):
                code[k] = False
            i = j
            continue
        if two == "/*":
            depth, j = 1, i + 2
            while j < n and depth:
                if text[j:j + 2] == "/*":
                    depth, j = depth + 1, j + 2
                elif text[j:j + 2] == "*/":
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            for k in range(i, j):
                code[k] = False
            i = j
            continue
        m = re.match(r'(b|c)?r(#*)"', text[i:i + 300]) if c in "bcr" and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")) else None
        if m:
            hashes = m.group(2)
            end = text.find('"' + hashes, i + m.end())
            j = n if end < 0 else end + 1 + len(hashes)
            for k in range(i, j):
                code[k] = False
            i = j
            continue
        if c == '"' or (c in "bc" and text[i + 1:i + 2] == '"' and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_"))):
            j = i + (1 if c == '"' else 2)
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            j += 1
            for k in range(i, min(j, n)):
                code[k] = False
            i = j
            continue
        if c == "'":
            # A char literal ('x', '\n', '\u{..}') or a lifetime ('a). Lifetimes stay code.
            m = re.match(r"'(\\u\{[0-9a-fA-F]+\}|\\.|[^\\'\n])'", text[i:i + 12])
            if m:
                for k in range(i, i + m.end()):
                    code[k] = False
                i += m.end()
                continue
        i += 1
    return code


def split_lines(text):
    """(line text, code mask) per line, without the newlines."""
    mask = scan(text)
    out, start = [], 0
    for idx, ch in enumerate(text):
        if ch == "\n":
            out.append((text[start:idx], mask[start:idx]))
            start = idx + 1
    if start < len(text):
        out.append((text[start:], mask[start:]))
    return out


def code_part(line, mask):
    """The line with comments and string contents left in, but ending at the last code character."""
    last = -1
    for k, ch in enumerate(line):
        if mask[k] and not ch.isspace():
            last = k
    return last


def indent_of(line):
    return len(line) - len(line.lstrip(" "))


def is_comment_or_attr(line, mask):
    s = line.strip()
    if not s:
        return False
    first = len(line) - len(line.lstrip(" "))
    return (not mask[first]) or s.startswith("#[") or s.startswith("#![")


def kind(line):
    s = line.strip()
    for word, group in (("let", "let"), ("if", "if"), ("return", "return"), ("match", "match"), ("for", "loop"), ("while", "loop"), ("loop", "loop")):
        if s == word or s.startswith(word + " ") or s.startswith(word + "("):
            return group
    return "other"


def move_braces(lines):
    """One pass; style() repeats it until nothing changes (each pass re-scans strings and comments)."""
    out = []
    for line, mask in lines:
        stripped = line.strip()
        ind = " " * indent_of(line)
        last = code_part(line, mask)
        if last < 0:
            out.append(line)
            continue
        # "} else {" / "} else if x {": the closer gets its own line
        m = re.match(r"^(\s*)\}[ \t]+(else\b.*)$", line)
        if m and mask[len(m.group(1))]:
            out.append(ind + "}")
            out.append(ind + m.group(2))
            continue
        k = last
        while k >= 0 and mask[k] and line[k] in OPENERS:
            k -= 1
        run = line[k + 1:last + 1]
        head = line[:k + 1].rstrip()
        after = line[last + 1:].strip()
        # only when nothing but a // comment follows (not a string that carries on)
        if after and not after.startswith("//"):
            out.append(line)
            continue
        if run and head.strip():
            tail = line[last + 1:]
            out.append(head + (" " + tail.strip() if tail.strip() else ""))
            out.append(ind + run)
        else:
            out.append(line)
    return out


def first_code_char(line, mask):
    for k, ch in enumerate(line):
        if not ch.isspace():
            return ch if mask[k] else None
    return None


def blank_lines(lines):
    out = []
    n = len(lines)
    for idx, (line, mask) in enumerate(lines):
        out.append((line, mask))
        if idx + 1 >= n:
            continue
        nxt, nmask = lines[idx + 1]
        if not nxt.strip():
            continue
        s = line.strip()
        # Rule 4: a line that only closes things (}, }), };, }, ...) gets a blank line after it.
        if s and first_code_char(line, mask) in tuple(CLOSERS) and re.fullmatch(r"[\}\)\]][\}\)\]\(\)?]*[;,]?", s):
            ns = nxt.strip()
            if first_code_char(nxt, nmask) in tuple(CLOSERS) or re.match(r"^(\.|\?|=>|as\b)", ns):
                continue
            out.append(("", []))
    return out


def grouping(lines):
    """Rule 6: a blank line wherever the kind of statement changes, at the same indent in the same block."""
    out = []
    for idx, (line, mask) in enumerate(lines):
        if line.strip() and not is_comment_or_attr(line, mask) and first_code_char(line, mask) not in tuple(CLOSERS) + ("{", "[", "("):
            k_now = kind(line)
            ind = indent_of(line)
            # the previous statement start at this indent, in the same block
            j = len(out) - 1
            # step over comments/attributes directly above (they stay with this line)
            top = len(out)
            while j >= 0 and out[j][0].strip() and is_comment_or_attr(*out[j]):
                top = j
                j -= 1
            prev = None
            if j >= 0 and out[j][0].strip():
                pl, pm = out[j]
                pc = code_part(pl, pm)
                ends_stmt = pc >= 0 and pl[pc] in ";}"
                if indent_of(pl) == ind and ends_stmt and first_code_char(pl, pm) not in tuple(CLOSERS):
                    prev = kind(pl)
                elif indent_of(pl) == ind and ends_stmt:
                    # the statement above ends on a closer line ("};", ").await?;"): find where it started
                    q = j
                    while q >= 0 and (not out[q][0].strip() or indent_of(out[q][0]) > ind or (indent_of(out[q][0]) == ind and (first_code_char(*out[q]) or "x") in "{[()]}")):
                        q -= 1
                    if q >= 0 and indent_of(out[q][0]) == ind and not is_comment_or_attr(*out[q]):
                        prev = kind(out[q][0])
                elif indent_of(pl) > ind and ends_stmt:
                    # a statement over several lines: find where it started
                    q = j
                    while q >= 0 and (not out[q][0].strip() or indent_of(out[q][0]) > ind):
                        q -= 1
                    if q >= 0 and indent_of(out[q][0]) == ind and not is_comment_or_attr(*out[q]) and first_code_char(*out[q]) not in tuple(CLOSERS):
                        prev = kind(out[q][0])
            if prev is not None:
                need = prev != k_now
                if need:
                    out.insert(top, ("", []))
        out.append((line, mask))
    return out


def tidy(lines):
    """No double blank lines, no blank line right after an opener or right before a closer."""
    out = []
    for idx, (line, mask) in enumerate(lines):
        if not line.strip():
            if not out or not out[-1][0].strip():
                continue
            if out[-1][0].strip() in ("{", "[", "(", "({", "[{"):
                continue
            nxt = next((l for l, m in lines[idx + 1:] if l.strip()), None)
            if nxt is None:
                continue
            ns = nxt.strip()
            if ns[:1] in CLOSERS:
                continue
            line, mask = "", []
        out.append((line.rstrip() if line.strip() else "", mask))
    return out


def style(text):
    while True:
        moved = "\n".join(move_braces(split_lines(text))) + "\n"
        if moved == text:
            break
        text = moved
    lines = split_lines(text)
    lines = blank_lines(lines)
    lines = grouping(lines)
    lines = tidy(lines)
    return "\n".join(l for l, _ in lines).rstrip("\n") + "\n"


def main():
    check = "--check" in sys.argv
    root = pathlib.Path(__file__).resolve().parent.parent
    changed = []
    for path in sorted(root.glob("**/*.rs")):
        if "target" in path.parts:
            continue
        before = path.read_text(encoding="utf-8")
        after = style(before)
        if after != before:
            changed.append(path.relative_to(root))
            if not check:
                path.write_text(after, encoding="utf-8")
    for p in changed:
        print(("would change: " if check else "styled: ") + str(p))
    if check and changed:
        sys.exit(1)


if __name__ == "__main__":
    main()
