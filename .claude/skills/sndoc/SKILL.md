---
name: sndoc
description: Search and fetch official ServiceNow product documentation as Markdown via the `sndoc` CLI. Use whenever a docs.servicenow.com URL appears (in user input, a web-search result, or any other source), or the user asks for ServiceNow API reference, documentation, or how-to content.
---

# ServiceNow docs via `sndoc`

`sndoc` is a local-first CLI that searches and fetches the official ServiceNow
product documentation (hybrid semantic + keyword search over the
`ServiceNow/ServiceNowDocs` mirror) and returns clean Markdown. Prefer it over a
raw web fetch for any ServiceNow docs request.

## When to use
- A `docs.servicenow.com` URL appears in the input, a web-search result, or elsewhere.
- The user asks for ServiceNow API reference or documentation content.
- The user wants to find a ServiceNow docs topic by keyword.

## Commands

```bash
# Search by keyword — hybrid search over the latest release.
# Each hit includes a repo `path` (use it with `fetch`) and a citation URL.
sndoc search "GlideRecord query"
sndoc search "GlideRecord query" --limit 5 --json   # structured output for parsing

# Fetch a topic as Markdown by its repo path (paths come from `search` output).
sndoc fetch administer/reference-pages/concept/c_GlideRecordQueries.md
sndoc fetch <path> --version tokyo                  # pin a specific release

# Fetch from a docs.servicenow.com URL or an `r/...` reader path.
sndoc fetch-url https://www.servicenow.com/docs/r/.../c_GlideRecord
sndoc fetch-url r/.../c_GlideRecord

# List available releases, newest first (latest is flagged).
sndoc list-versions
sndoc list-versions --json
```

Add `--json` to any read command (`search`, `fetch`, `fetch-url`, `list-versions`)
for structured output that's easy to parse programmatically.

## Output
Markdown is written to **stdout** (with a `> Source: <url>` citation line); progress
and diagnostics go to **stderr**. Save with `sndoc <args> > out.md`, or use `--json`
when you need to parse the result.

## Workflow

### When you have a URL
Pass it straight to `fetch-url` — it accepts both full `docs.servicenow.com` URLs and
bare `r/...` reader paths:
```bash
sndoc fetch-url https://www.servicenow.com/docs/r/.../c_GlideRecord
```

### When you only have a topic name
1. `sndoc search "<topic>"` — pick the most relevant hit and note its `path`.
2. `sndoc fetch <path>` — fetch that topic as Markdown.

## Version rule
Releases are discovered dynamically and ordered **newest-first** (the most recent is
flagged as latest). Never guess or hardcode a release name — run `sndoc list-versions`
to see what's available, and use `--version <release>` only when the user asks for a
specific older release.
