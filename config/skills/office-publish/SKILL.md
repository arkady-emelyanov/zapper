---
name: office-publish
description: How to publish work to the firm's shared drive and use others' work (kinds, titles, summaries, versions, audience, attachments, Markdown and slide format); use it whenever you deliver or build on a deliverable.
---

# Publishing to the shared drive

The shared drive is where the firm's finished work lives. Your `/work` directory is your own disk: nobody else can see it, and it is gone when the run ends. What you publish stays, can be found by everyone in its audience, and cannot be changed after publishing.

## When to publish

- Publish deliverables: what an assignment asked for, a document others will build on, a result the board or your manager will read.
- Do not publish scratch: notes to yourself, intermediate files, logs, partial drafts nobody asked to see.
- Publish before you report. Your report mail attaches the artifact; the body summarises it.
- Product code lives in the firm's git repository, not on the drive. Publish code only when asked, or for code that is not part of the product.

## The tools

- `drive_publish(paths, title, kind, summary, audience?, artifact?, thread?)`: copies files out of your VM. Paths must be under `/work`, your home or `/tmp`. About 50 MiB per artifact.
- `drive_list(kind?, author?, query?)`: find artifacts.
- `drive_fetch(artifact, dest)`: copy an artifact (`a-3` for the latest, `a-3@v2` for a version) into your VM, e.g. `dest: "/work/in/a-3"`.

## Choosing kind

| kind | For |
|---|---|
| `document` | Briefs, specs, designs, findings, guides: Markdown meant to be read and referred to |
| `report` | Results of a piece of work or a period: what was done, outcome, evidence |
| `code` | A snapshot of files or a git bundle, when asked |
| `dataset` | CSV or JSON data; CSV with a header row is preferred |
| `deck` | Slides for a presentation |
| `board-pack` | The CEO's report to the board, as slides |

## Title, summary, audience, thread

- Title: specific and short. "Product brief v1: scope and acceptance criteria", not "Brief".
- Summary: one paragraph, shown in lists and mail. Say what it is, the key conclusion or result, and what it is for. A reader who sees only the summary should know whether to open it.
- `audience`: `firm` (default) for most work; `board` for what is meant for the board; a list of names when only they should see it.
- `thread`: the thread of the assignment it answers (`t-12`). This links the work to the request in the UI.

## Revising: new version, not new artifact

When you revise something you already published, publish again with `artifact: "a-3"`. That adds version 2 under the same id, so links and history stay together. Create a new artifact only for genuinely different work. Say what changed in the summary ("v2: added acceptance criteria for X; removed Y").

## Attaching to mail

Attach as `drive:a-3@v2` (with the version) in `attachments`. Name the version you mean, so a later revision does not change what you referred to. In the body, say what the reader should look at.

```
drive_publish(paths: ["/work/brief.md"], title: "Product brief v1", kind: "document",
  summary: "...", thread: "t-12")
-> a-3@v1
mail_reply(message: "m-40", kind: "report", close: true,
  attachments: ["drive:a-3@v1"], body: "Brief published. Key decisions: ...")
```

## Building on others' work

- Fetch before you build on something: `drive_fetch(artifact: "a-3@v2", dest: "/work/in/a-3")`, and read it. Do not work from a summary or from memory of an earlier version.
- When you received an attachment by mail, fetch exactly that version.
- Use `drive_list(query: ...)` to check whether something already exists before you produce it again.
- Cite what you used by id and version in your own work.

## Markdown conventions

The UI renders: headings, paragraphs, emphasis, inline code and code blocks, lists, block quotes, tables, horizontal rules, links, and images that are files of the same artifact. Raw HTML is shown as text, so do not use it.

- Start with a `#` title, then a short summary paragraph, then sections with `##`.
- Put the conclusion first. Detail and method after.
- Tables for comparisons and numbers. Code blocks for commands and output.
- Links: only `http`, `https` and `drive:` links are clickable. Refer to other artifacts as `drive:a-3@v2`.
- Images: publish the image file in the same artifact (`paths: ["/work/report.md", "/work/chart.png"]`) and reference it by relative path: `![Requests per minute](chart.png)`.
- Write each paragraph and list item on one line.

## Screenshots

Show, don't describe: `browse shot http://apps.<domain>/ /work/pack/home.png` captures the product (`--width 390` for a phone). Put screenshots next to the Markdown in the same artifact and reference them by relative path (`![Home](home.png)`); the UI shows images that are files of the same artifact.

## Decks and board packs

Markdown slides, one slide per section, separated by a line containing only `---`.

```
# Title of the deck
Subtitle, author, date

---

## One idea per slide

- Three to five bullets
- Short phrases, numbers where possible

---

## Metrics

| Metric | Value |
|---|---|
| ... | ... |
```

- One idea per slide, a heading on every slide.
- At most about six bullets or one table per slide.
- Put the full detail in a companion document and link it, rather than crowding slides.
