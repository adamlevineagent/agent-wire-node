# Knowledge (corpora and local sync)

The **Knowledge** mode is the document-management side of Wire Node. Where Understanding is about pyramids (layered evidence graphs over material), Knowledge is about the material itself: documents, corpora, and the folders on your disk that contain them.

You can build pyramids without ever touching Knowledge — just point the Add Workspace wizard at a folder. Knowledge is for when you want to treat documents as first-class citizens: organize them into corpora, version them, sync them from remote sources, or make a curated collection queryable.

---

## Corpora

A **corpus** is a named collection of documents. It has:

- A **title** and a **description**.
- A **material class** — documents / code / conversations / mixed.
- A **visibility** — public / unlisted / private.
- A set of **documents** — with per-document status (published / draft / retracted).
- An optional **revenue** accounting (if you price corpus documents on the Wire).

Corpora are how you say "these 40 PDFs belong together as a coherent collection." You can build a pyramid over a corpus directly without physically co-locating the files in a single folder — corpora are a logical grouping that the build pipeline can resolve.

### The Corpora tab

Click **Knowledge → Corpora** in the sidebar. You see cards, one per corpus:

- Title and description.
- Visibility badge.
- Document counts (published / draft / retracted / total).
- Revenue earned (if applicable).

Click a corpus to open the detail view. There you manage individual documents, change visibility, and configure publication.

### Creating a corpus

From the Corpora tab, click **Create corpus**. Enter title, description, material class. Save. The corpus is empty; add documents to it next.

### Adding documents to a corpus

Two paths:

- **Upload** — pick files from your disk. They get copied into the corpus cache.
- **Link from a folder** — point at a folder synced via Local Sync (see below). Documents in that folder belong to the corpus.

Either way, each document gets a status:

- **Draft** — present in the corpus but not yet published.
- **Published** — available on the Wire if the corpus is public.
- **Retracted** — previously published, now withdrawn.

## Local Sync

The **Local Sync** tab in Knowledge is where you manage folders Wire Node watches for document changes. It's similar to the folder-link mechanism that Understanding uses for pyramid building, but oriented around document-level sync rather than pyramid-level builds.

### Linked folders

Each linked folder shows:

- **Path** on your disk.
- **Last synced** time.
- **Auto-sync** on/off — should Wire Node watch this folder and pick up changes automatically.
- **Interval** — how often to check (if auto-sync is on).

Actions per folder:

- **Sync now** — manual sync.
- **Remove** — unlink the folder (does not delete files on disk).

### Cached documents

Below the folder list, a grouped view of all documents synced across all folders, organized by corpus. Each document shows:

- File name.
- Size.
- Last synced.
- Number of versions retained.
- Associated corpus (if any).

Click a document to see its version history.

### Version history

Wire Node retains multiple versions of a synced document so you can see what changed and when. Each version has:

- Timestamp.
- Size.
- SHA-256 hash.
- Optional author (if the sync source provides one).

You can **diff** any two versions — a side-by-side view with highlighted changes. Useful for documents under active editing.

---

## Why corpora and linked folders are separate

A corpus is a **logical grouping** (a set of documents that belong together, with publication semantics). A linked folder is a **physical location** on disk (files you want to sync).

They overlap but aren't the same:

- You can have a linked folder whose files are *not* in any corpus (they're just synced, not grouped).
- You can have a corpus whose documents come from multiple linked folders (or from uploads with no folder at all).

Most users set up one linked folder per project and one corpus per project, and treat them as paired. But the separation lets you do more sophisticated things — e.g. a corpus that gathers the "best of" several linked folders.

---

## Publishing a corpus

If your corpus has visibility set to public or unlisted, you can publish documents from it to the Wire. From the corpus detail view:

- Set the corpus visibility.
- For each document, mark it as published or keep it as draft.
- Optionally set per-document pricing (for emergent-access corpora).

Published corpus documents become citable by handle-path. Pyramids built elsewhere can cite them as source.

See [`61-publishing.md`](61-publishing.md).

---

## How Knowledge relates to pyramids

Building a pyramid over a corpus:

1. In Understanding, click **Add Workspace**.
2. Instead of picking a folder, pick **Corpus as source**.
3. Choose the corpus.
4. Continue as normal.

The build treats the corpus's documents as its source layer. If the corpus changes (documents added, versions updated), DADBEAR picks up the change and re-evaluates affected nodes.

This lets you:

- Maintain one curated corpus and build multiple pyramids over it with different apex questions.
- Publish the corpus and publish pyramids over it; readers can cite either the source or the structured understanding.
- Keep corpus documents in version control cleanly, with a dedicated pyramid-building layer on top.

---

## Mesh hosting (optional)

If you turned on mesh hosting during onboarding (or in Settings), your node hosts documents from published corpora — both yours and others — to help the Wire distribute load.

Mesh hosting:

- Uses disk space (up to your configured cap).
- Earns you mesh hosting credits (a small trickle for files you serve).
- Is independent of the compute market (that's about LLM serving, not document serving).

See [`32-settings.md`](32-settings.md) for how to configure mesh hosting.

---

## Common patterns

**"I have a folder of PDFs and I just want to query them."** — Skip Knowledge entirely. Go to Understanding → Add Workspace → pick the folder → content type `document`. Done.

**"I have a research archive I'm going to build many pyramids over."** — Create a corpus. Link the folder as auto-sync. Build pyramids against the corpus, not the folder — they'll track corpus changes cleanly.

**"I'm publishing research; I want the raw documents and the structured pyramid both available."** — Create a public corpus, publish documents as they're ready, build and publish pyramids over the corpus, cite the corpus as source.

**"I want to archive a snapshot of some material."** — Create a corpus, upload the files, leave visibility unlisted, never update. DADBEAR on any pyramid over it will effectively idle since source doesn't change.

---

## Where to go next

- [`20-pyramids.md`](20-pyramids.md) — building over corpora.
- [`61-publishing.md`](61-publishing.md) — publishing corpora and documents.
- [`90-data-layout.md`](90-data-layout.md) — where corpus and document data lives on disk.
