#!/usr/bin/env python3
"""Puts the canonical AI prompt into the built landing page (called by `make landing-build`).

The prompt that asks an AI assistant for a program lives in one file, `programs/ai-prompt.md`
(#108): the app shows it and the landing page shows it. This script replaces the text between the
`<!-- prompt:begin -->` and `<!-- prompt:end -->` markers of the built index.html with that file,
HTML-escaped. The page has no copy of its own: the build fails if the file is missing or empty.

Usage: landing-prompt.py <built index.html> <prompt file>
"""
import html
import pathlib
import sys

BEGIN, END = "<!-- prompt:begin -->", "<!-- prompt:end -->"


def main() -> int:
    page_path, prompt_path = map(pathlib.Path, sys.argv[1:3])
    page = page_path.read_text(encoding="utf-8")
    if page.count(BEGIN) != 1 or page.count(END) != 1 or page.index(BEGIN) > page.index(END):
        print(f"{page_path}: expected one {BEGIN} ... {END} pair", file=sys.stderr)
        return 1
    if not prompt_path.exists():
        print(f"{prompt_path} not found: the landing page needs the AI prompt", file=sys.stderr)
        return 1
    prompt = prompt_path.read_text(encoding="utf-8").strip()
    if not prompt:
        print(f"{prompt_path} is empty", file=sys.stderr)
        return 1
    start = page.index(BEGIN) + len(BEGIN)
    page = page[:start] + html.escape(prompt, quote=False) + page[page.index(END):]
    page_path.write_text(page, encoding="utf-8")
    print(f"Prompt: {prompt_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
