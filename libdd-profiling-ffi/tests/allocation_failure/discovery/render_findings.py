#!/usr/bin/env python3
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

"""Render `FINDINGS.md` to a self-contained `FINDINGS.html` (inline CSS, no remote assets).

Supports the Markdown subset `FINDINGS.md` uses: ATX headings, paragraphs, `-` and `1.` lists
(two-space nesting), pipe tables, fenced code blocks, inline code, bold and links. Every `##`
and `###` heading opens a `<section>` with a stable id, and `##` headings feed the navigation.

  render_findings.py           write FINDINGS.html
  render_findings.py --check   fail if FINDINGS.html is not what this script would write
"""

import argparse
import html
import re
import sys
from pathlib import Path

HARNESS = Path(__file__).resolve().parent.parent
SOURCE = HARNESS / "FINDINGS.md"
TARGET = HARNESS / "FINDINGS.html"

CSS = """
:root { --fg: #1f2328; --muted: #59636e; --border: #d1d9e0; --code: #f6f8fa; --accent: #632ca6; }
* { box-sizing: border-box; }
body { margin: 0; color: var(--fg); font: 15px/1.55 -apple-system, "Segoe UI", Helvetica, Arial, sans-serif; }
.layout { display: grid; grid-template-columns: 18rem 1fr; }
nav { position: sticky; top: 0; height: 100vh; overflow-y: auto; padding: 1.5rem 1rem; border-right: 1px solid var(--border); background: #fbfbfd; font-size: 13px; }
nav ol { list-style: none; padding-left: 0; margin: 0; }
nav ol ol { padding-left: 0.9rem; }
nav li { margin: 0.2rem 0; }
nav a { color: var(--fg); text-decoration: none; }
nav a:hover { color: var(--accent); text-decoration: underline; }
main { max-width: 64rem; padding: 1.5rem 2.5rem 4rem; }
h1 { font-size: 1.9rem; margin-top: 0; }
h2 { font-size: 1.45rem; margin-top: 2.4rem; padding-bottom: 0.3rem; border-bottom: 1px solid var(--border); }
h3 { font-size: 1.15rem; margin-top: 2rem; }
h4 { font-size: 1rem; margin-top: 1.4rem; }
section.finding { border-left: 4px solid var(--accent); padding: 0.1rem 0 0.4rem 1rem; margin: 1.6rem 0; }
a { color: var(--accent); }
code { font: 0.88em ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; background: var(--code); padding: 0.1em 0.3em; border-radius: 4px; overflow-wrap: anywhere; }
pre { background: var(--code); padding: 0.8rem 1rem; border-radius: 6px; overflow-x: auto; }
pre code { background: none; padding: 0; overflow-wrap: normal; }
table { border-collapse: collapse; margin: 0.8rem 0; display: block; overflow-x: auto; }
th, td { border: 1px solid var(--border); padding: 0.35rem 0.6rem; text-align: left; vertical-align: top; }
th { background: var(--code); }
td.num, th.num { text-align: right; }
@media (max-width: 900px) { .layout { display: block; } nav { position: static; height: auto; border-right: 0; border-bottom: 1px solid var(--border); } main { padding: 1rem; } }
@media print {
  .layout { display: block; }
  nav { position: static; height: auto; border: 0; background: none; page-break-after: always; }
  main { max-width: none; padding: 0; }
  a { color: inherit; }
  a[href^="http"]::after { content: " (" attr(href) ")"; font-size: 0.8em; color: var(--muted); }
  section.finding, table, pre { break-inside: avoid; }
  h2, h3 { break-after: avoid; }
}
"""


def slug(text, seen):
    base = re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-") or "section"
    candidate, n = base, 2
    while candidate in seen:
        candidate, n = f"{base}-{n}", n + 1
    seen.add(candidate)
    return candidate


def inline(text):
    """Escape, then render code spans, links and bold. Code spans are not processed further."""
    out, pos = [], 0
    for m in re.finditer(r"`([^`]+)`", text):
        out.append(inline_plain(text[pos : m.start()]))
        out.append(f"<code>{html.escape(m.group(1))}</code>")
        pos = m.end()
    out.append(inline_plain(text[pos:]))
    return "".join(out)


def inline_plain(text):
    parts, pos = [], 0
    for m in re.finditer(r"\[([^\]]+)\]\(([^)\s]+)\)", text):
        parts.append(emphasis(html.escape(text[pos : m.start()])))
        label = inline(m.group(1))
        parts.append(f'<a href="{html.escape(m.group(2), quote=True)}">{label}</a>')
        pos = m.end()
    parts.append(emphasis(html.escape(text[pos:])))
    return "".join(parts)


def emphasis(escaped):
    return re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", escaped)


def table(rows):
    def cells(row):
        return [c.strip() for c in row.strip().strip("|").split("|")]

    head, align, body = cells(rows[0]), cells(rows[1]), [cells(r) for r in rows[2:]]
    num = [a.endswith(":") and not a.startswith(":") for a in align]

    def cls(i):
        return ' class="num"' if i < len(num) and num[i] else ""

    out = ["<table>", "<thead><tr>"]
    out += [f"<th{cls(i)}>{inline(c)}</th>" for i, c in enumerate(head)]
    out += ["</tr></thead>", "<tbody>"]
    for r in body:
        out.append("<tr>" + "".join(f"<td{cls(i)}>{inline(c)}</td>" for i, c in enumerate(r)) + "</tr>")
    out += ["</tbody>", "</table>"]
    return "\n".join(out)


LIST_ITEM = re.compile(r"^( *)(-|\d+\.) (.*)$")


def render(markdown):
    lines = markdown.splitlines()
    body, toc, seen = [], [], set()
    open_sections = []  # heading levels of the open <section> elements
    title = "Findings"
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.startswith("```"):
            j = i + 1
            while j < len(lines) and not lines[j].startswith("```"):
                j += 1
            lang = line[3:].strip()
            attr = f' class="language-{html.escape(lang)}"' if lang else ""
            body.append(f"<pre><code{attr}>{html.escape(chr(10).join(lines[i + 1 : j]))}</code></pre>")
            i = j + 1
            continue
        m = re.match(r"^(#{1,4}) (.*)$", line)
        if m:
            level, text = len(m.group(1)), m.group(2).strip()
            if level == 1:
                title = re.sub(r"`", "", text)
                body.append(f"<h1>{inline(text)}</h1>")
            else:
                ident = slug(re.sub(r"`", "", text), seen)
                if level <= 3:
                    while open_sections and open_sections[-1] >= level:
                        open_sections.pop()
                        body.append("</section>")
                    finding = level == 3 and re.match(r"^F\d+\b", text)
                    cls = ' class="finding"' if finding else ""
                    body.append(f'<section id="{ident}"{cls}>')
                    open_sections.append(level)
                    toc.append((level, ident, text))
                body.append(f'<h{level} id="h-{ident}">{inline(text)}</h{level}>')
            i += 1
            continue
        if line.startswith("|"):
            j = i
            while j < len(lines) and lines[j].startswith("|"):
                j += 1
            body.append(table(lines[i:j]))
            i = j
            continue
        if LIST_ITEM.match(line) and not line.startswith(" "):
            j = i + 1
            while j < len(lines) and lines[j].strip() and not lines[j].startswith(("#", "|", "```")):
                j += 1
            body.append(list_html(lines[i:j]))
            i = j
            continue
        if not line.strip():
            i += 1
            continue
        j = i
        while j < len(lines) and lines[j].strip() and not re.match(r"^(#{1,4} |\||```|- |\d+\. )", lines[j]):
            j += 1
        body.append(f"<p>{inline(' '.join(l.strip() for l in lines[i:j]))}</p>")
        i = j
    while open_sections:
        open_sections.pop()
        body.append("</section>")
    return page(title, toc, "\n".join(body))


def list_html(lines):
    """Render a list block with proper nesting: a nested list goes inside its parent `<li>`."""
    items = []  # (indent, tag, text)
    for line in lines:
        m = LIST_ITEM.match(line)
        if m:
            tag = "ol" if m.group(2)[0].isdigit() else "ul"
            items.append([len(m.group(1)), tag, m.group(3)])
        else:
            items[-1][2] += " " + line.strip()
    out, stack = [], []
    for indent, tag, text in items:
        if not stack or indent > stack[-1][0]:
            out.append(f"<{tag}>")
            stack.append((indent, tag))
        else:
            out.append("</li>")
            while stack and indent < stack[-1][0]:
                out.append(f"</{stack.pop()[1]}>")
                out.append("</li>")
        out.append(f"<li>{inline(text)}")
    out.append("</li>")
    while stack:
        out.append(f"</{stack.pop()[1]}>")
        if stack:
            out.append("</li>")
    return "".join(out)


def page(title, toc, body):
    nav, depth = ["<ol>"], 2
    for level, ident, text in toc:
        while level > depth:
            nav[-1] = nav[-1][: -len("</li>")]
            nav.append("<ol>")
            depth += 1
        while level < depth:
            nav.append("</ol></li>")
            depth -= 1
        nav.append(f'<li><a href="#{ident}">{inline(text)}</a></li>')
    while depth > 2:
        nav.append("</ol></li>")
        depth -= 1
    nav.append("</ol>")
    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(title)}</title>
<style>{CSS}</style>
</head>
<body>
<div class="layout">
<nav aria-label="Contents">
<p><strong>Contents</strong></p>
<p>Source: <a href="FINDINGS.md">FINDINGS.md</a></p>
{chr(10).join(nav)}
</nav>
<main>
<!-- Generated from FINDINGS.md by discovery/render_findings.py. Do not edit: run it again. -->
{body}
</main>
</div>
</body>
</html>
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="fail if FINDINGS.html is stale")
    args = parser.parse_args()
    rendered = render(SOURCE.read_text(encoding="utf-8"))
    if args.check:
        current = TARGET.read_text(encoding="utf-8") if TARGET.exists() else ""
        if current != rendered:
            sys.exit(f"{TARGET.name} is stale: run {Path(__file__).name}")
        return
    TARGET.write_text(rendered, encoding="utf-8")


if __name__ == "__main__":
    main()
