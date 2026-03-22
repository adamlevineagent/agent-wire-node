#!/usr/bin/env npx tsx
/**
 * backfill-source-paths.ts
 *
 * One-time backfill: matches remote Wire documents to local files by body_hash,
 * then PATCHes source_path on documents that are missing it.
 *
 * Usage:
 *   API_URL=https://newsbleach.com API_TOKEN=wire_... npx tsx scripts/backfill-source-paths.ts
 *
 * Options:
 *   --dry-run   Print what would be patched without making changes
 */

import { createHash } from 'node:crypto';
import { readdir, readFile, stat } from 'node:fs/promises';
import { join, relative } from 'node:path';

// ── Config ──────────────────────────────────────────────────

const API_URL = process.env.API_URL;
const API_TOKEN = process.env.API_TOKEN;
const CORPUS_SLUG = 'agent-wire-canon';
const LOCAL_DOCS_DIR = join(__dirname, '..', '..', 'GoodNewsEveryone', 'docs');
const DRY_RUN = process.argv.includes('--dry-run');

if (!API_URL || !API_TOKEN) {
  console.error('ERROR: API_URL and API_TOKEN env vars are required.');
  console.error('Usage: API_URL=https://... API_TOKEN=wire_... npx tsx scripts/backfill-source-paths.ts');
  process.exit(1);
}

// ── Types ───────────────────────────────────────────────────

interface RemoteDocument {
  id: string;
  title: string;
  body_hash: string;
  source_path: string | null;
}

interface LocalFile {
  relativePath: string;
  bodyHash: string;
}

// ── Local scanning ──────────────────────────────────────────

function computeSha256(content: string): string {
  return createHash('sha256').update(content, 'utf-8').digest('hex');
}

async function scanLocalFiles(rootDir: string): Promise<LocalFile[]> {
  const results: LocalFile[] = [];

  async function walk(dir: string) {
    const entries = await readdir(dir, { withFileTypes: true });
    for (const entry of entries) {
      // Skip hidden files/dirs
      if (entry.name.startsWith('.')) continue;
      // Skip .versions directories
      if (entry.name === '.versions') continue;

      const fullPath = join(dir, entry.name);

      if (entry.isDirectory()) {
        await walk(fullPath);
      } else if (entry.isFile()) {
        // Skip non-text files
        if (entry.name === 'Thumbs.db' || entry.name === 'desktop.ini') continue;

        try {
          const content = await readFile(fullPath, 'utf-8');
          const bodyHash = computeSha256(content);
          const relativePath = relative(rootDir, fullPath).replace(/\\/g, '/');
          results.push({ relativePath, bodyHash });
        } catch (err) {
          console.warn(`  WARN: Could not read ${fullPath}: ${err}`);
        }
      }
    }
  }

  await walk(rootDir);
  return results;
}

// ── Remote API ──────────────────────────────────────────────

async function fetchAllDocuments(): Promise<RemoteDocument[]> {
  const allDocs: RemoteDocument[] = [];
  let offset = 0;
  const limit = 100;

  while (true) {
    const url = `${API_URL}/api/v1/wire/corpora/${CORPUS_SLUG}/documents?limit=${limit}&offset=${offset}`;
    const resp = await fetch(url, {
      headers: { Authorization: `Bearer ${API_TOKEN}` },
    });

    if (!resp.ok) {
      const text = await resp.text();
      throw new Error(`Failed to fetch documents (${resp.status}): ${text}`);
    }

    const data = await resp.json() as { items: RemoteDocument[]; total: number };
    allDocs.push(...data.items);

    if (allDocs.length >= data.total || data.items.length === 0) break;
    offset += limit;
  }

  return allDocs;
}

async function patchSourcePath(docId: string, sourcePath: string): Promise<boolean> {
  const url = `${API_URL}/api/v1/wire/documents/${docId}`;
  const resp = await fetch(url, {
    method: 'PATCH',
    headers: {
      Authorization: `Bearer ${API_TOKEN}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ source_path: sourcePath }),
  });

  if (!resp.ok) {
    const text = await resp.text();
    console.error(`  FAIL: PATCH ${docId} -> ${sourcePath}: ${resp.status} ${text}`);
    return false;
  }
  return true;
}

// ── Main ────────────────────────────────────────────────────

async function main() {
  console.log(`Backfill source_path for corpus: ${CORPUS_SLUG}`);
  console.log(`Local docs dir: ${LOCAL_DOCS_DIR}`);
  console.log(`API: ${API_URL}`);
  if (DRY_RUN) console.log('*** DRY RUN — no changes will be made ***');
  console.log();

  // 1. Scan local files
  console.log('Scanning local files...');
  const localFiles = await scanLocalFiles(LOCAL_DOCS_DIR);
  console.log(`  Found ${localFiles.length} local files`);

  // 2. Fetch remote documents
  console.log('Fetching remote documents...');
  const remoteDocs = await fetchAllDocuments();
  console.log(`  Found ${remoteDocs.length} remote documents`);
  console.log();

  // 3. Build lookup maps
  const localByHash = new Map<string, LocalFile[]>();
  for (const f of localFiles) {
    const existing = localByHash.get(f.bodyHash) || [];
    existing.push(f);
    localByHash.set(f.bodyHash, existing);
  }

  const remoteByHash = new Map<string, RemoteDocument[]>();
  for (const d of remoteDocs) {
    const existing = remoteByHash.get(d.body_hash) || [];
    existing.push(d);
    remoteByHash.set(d.body_hash, existing);
  }

  // 4. Match and patch
  let patched = 0;
  let alreadySet = 0;
  let matched = 0;
  const unmatchedRemote: RemoteDocument[] = [];
  const unmatchedLocal: LocalFile[] = [];
  const patchErrors: string[] = [];

  for (const doc of remoteDocs) {
    const locals = localByHash.get(doc.body_hash);
    if (!locals || locals.length === 0) {
      unmatchedRemote.push(doc);
      continue;
    }

    matched++;

    // Use the first matching local file
    const localFile = locals[0];

    if (doc.source_path && doc.source_path.length > 0) {
      alreadySet++;
      continue;
    }

    if (DRY_RUN) {
      console.log(`  WOULD PATCH: ${doc.id} (${doc.title}) -> ${localFile.relativePath}`);
      patched++;
    } else {
      const ok = await patchSourcePath(doc.id, localFile.relativePath);
      if (ok) {
        console.log(`  PATCHED: ${doc.id} (${doc.title}) -> ${localFile.relativePath}`);
        patched++;
      } else {
        patchErrors.push(`${doc.id}: ${doc.title}`);
      }
    }
  }

  for (const f of localFiles) {
    const remotes = remoteByHash.get(f.bodyHash);
    if (!remotes || remotes.length === 0) {
      unmatchedLocal.push(f);
    }
  }

  // 5. Report
  console.log();
  console.log('═══════════════════════════════════════════');
  console.log('  RESULTS');
  console.log('═══════════════════════════════════════════');
  console.log(`  Total local files:      ${localFiles.length}`);
  console.log(`  Total remote documents: ${remoteDocs.length}`);
  console.log(`  Matched by hash:        ${matched}`);
  console.log(`  Already had source_path:${alreadySet}`);
  console.log(`  ${DRY_RUN ? 'Would patch' : 'Patched'}:            ${patched}`);
  if (patchErrors.length > 0) {
    console.log(`  Patch errors:           ${patchErrors.length}`);
  }
  console.log();

  if (unmatchedRemote.length > 0) {
    console.log(`  Unmatched remote (${unmatchedRemote.length} docs — no local file with same hash):`);
    for (const d of unmatchedRemote) {
      console.log(`    - ${d.id}  ${d.title}  [${d.body_hash.slice(0, 12)}...]`);
    }
    console.log();
  }

  if (unmatchedLocal.length > 0) {
    console.log(`  Unmatched local (${unmatchedLocal.length} files — no remote doc with same hash):`);
    for (const f of unmatchedLocal) {
      console.log(`    - ${f.relativePath}  [${f.bodyHash.slice(0, 12)}...]`);
    }
    console.log();
  }

  if (patchErrors.length > 0) {
    console.log('  Failed patches:');
    for (const e of patchErrors) {
      console.log(`    - ${e}`);
    }
  }
}

main().catch((err) => {
  console.error('Fatal error:', err);
  process.exit(1);
});
