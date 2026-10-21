---
name: usage
description: Open the Reader Tick on original financial documents — earnings releases, filings, investor pages — from URLs or local HTML, Markdown, text or PDF files, such as a request to read a company's latest results release. Load it before opening tick-tickloop.
---

# Tickloop Reader Tick

`tick-tickloop` serves the official Tickloop Ticks as protocol Ticks. Open the Reader through the `tickloop` Skill's flow: `TickManage` with `action: "open"` and `command: ["tick-tickloop", "reader", DOCUMENT, ...]`. It opens as a protocol Tick by itself, so leave out `protocol`.

Each `DOCUMENT` is an HTTP(S) URL or a local HTML, Markdown, text or PDF file; give up to 32. A relative path resolves against the Tick's `cwd`. Add options only when they are known:

- `--symbol SYMBOL` names the company the documents report on, such as `AAPL.US`.
- `--fiscal-period PERIOD` names the reported period, such as `FY2026Q3`.
- `--published-at DATE` gives the publication date when the document does not state it.

Never add `--once` or `--format`: they print a snapshot and exit, which a Tick refuses. `tick-tickloop reader --help` lists every option.

The Reader needs no login. When a document cannot be fetched or read, the Tick ends with a diagnostic: report it and ask the user for another URL or file.

After opening an instance, use `TickDescribe` for its current tool catalog and revision, `TickRead` for its state, and `TickCall` for a declared operation. This Skill describes launch commands and prerequisites, not a second runtime capability catalog.
