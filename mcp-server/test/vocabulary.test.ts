/**
 * Phase 6c-C — Tests for the dynamic vocabulary fetcher in `lib.ts`.
 *
 * Covers:
 *   1. annotation_types_fetched_from_vocab_endpoint_on_startup
 *   2. zod_validation_accepts_vocab_published_types (via validateAnnotationType)
 *   3. zod_validation_rejects_unknown_type_with_helpful_message
 *   4. fallback_to_hardcoded_on_fetch_failure_preserves_genesis_types
 *   5. help_text_renders_current_vocab (via getAnnotationTypesSync + renderVocabTypeList)
 *
 * Runs against a local HTTP stub on port 8765 that mirrors the Wire node's
 * `GET /vocabulary/:vocab_kind` contract. Silences the fallback warning via
 * PYRAMID_MCP_QUIET.
 */

import { createServer } from 'node:http';
import { AddressInfo } from 'node:net';
import test, { after, afterEach } from 'node:test';
import assert from 'node:assert/strict';

type StubBehavior =
    | { kind: 'ok'; entries: Array<{ name: string; description?: string }> }
    | { kind: 'status'; status: number }
    | { kind: 'drop' };

// Bind the stub server to an ephemeral port and point lib.ts at it via
// the env override. MUST run before importing lib.ts so the module-scope
// WIRE_NODE_BASE_URL snapshot picks it up.
process.env.PYRAMID_MCP_QUIET = '1';

// We need a port before the import runs. `createServer(...).listen(0)`
// returns an ephemeral port, so we start the stub BEFORE importing lib.ts
// and set the env var once the port is known. Do it via top-level await.
const stubServerSetup = await (async () => {
    let currentBehavior: StubBehavior = { kind: 'ok', entries: [] };
    const server = createServer((req, res) => {
        if (!req.url || !req.url.startsWith('/vocabulary/')) {
            res.statusCode = 404;
            res.end();
            return;
        }
        const kind = decodeURIComponent(req.url.slice('/vocabulary/'.length));
        const b = currentBehavior;
        if (b.kind === 'drop') {
            req.socket.destroy();
            return;
        }
        if (b.kind === 'status') {
            res.statusCode = b.status;
            res.setHeader('content-type', 'application/json');
            res.end(JSON.stringify({ error: 'stub' }));
            return;
        }
        res.statusCode = 200;
        res.setHeader('content-type', 'application/json');
        res.end(
            JSON.stringify({
                vocab_kind: kind,
                entries: b.entries.map((e) => ({
                    name: e.name,
                    description: e.description ?? 'test',
                    handler_chain_id: null,
                    reactive: false,
                    creates_delta: false,
                })),
            }),
        );
    });
    await new Promise<void>((resolve, reject) => {
        server.once('error', reject);
        server.listen(0, '127.0.0.1', () => resolve());
    });
    const addr = server.address() as AddressInfo;
    process.env.PYRAMID_MCP_BASE_URL = `http://127.0.0.1:${addr.port}`;
    return {
        server,
        setBehavior(b: StubBehavior) {
            currentBehavior = b;
        },
    };
})();

import {
    FALLBACK_ANNOTATION_TYPES,
    getAnnotationTypes,
    getAnnotationTypesSync,
    refreshAnnotationTypes,
    validateAnnotationType,
    renderVocabTypeList,
} from '../src/lib.js';

// Helpers for driving the stub from tests.
function setBehavior(b: StubBehavior) {
    stubServerSetup.setBehavior(b);
}

after(async () => {
    await new Promise<void>((resolve) =>
        stubServerSetup.server.close(() => resolve()),
    );
});

afterEach(() => {
    setBehavior({ kind: 'ok', entries: [] });
});

// ── Tests ────────────────────────────────────────────────────────────────────

test('annotation_types_fetched_from_vocab_endpoint_on_startup', async () => {
    setBehavior({
        kind: 'ok',
        entries: [
            { name: 'observation' },
            { name: 'correction' },
            { name: 'custom_one' },
        ],
    });
    const names = await refreshAnnotationTypes();
    assert.ok(names.includes('observation'));
    assert.ok(names.includes('correction'));
    assert.ok(names.includes('custom_one'));
    assert.equal(names.length, 3);

    // Sync read after fetch returns same set
    const sync = getAnnotationTypesSync();
    assert.ok(sync);
    assert.deepEqual([...sync].sort(), [...names].sort());
});

test('zod_validation_accepts_vocab_published_types', async () => {
    setBehavior({
        kind: 'ok',
        entries: [{ name: 'observation' }, { name: 'my_custom_type' }],
    });
    await refreshAnnotationTypes();

    const ok = await validateAnnotationType('my_custom_type');
    assert.equal(ok.ok, true);
    if (ok.ok) {
        assert.equal(ok.name, 'my_custom_type');
    }

    const ok2 = await validateAnnotationType('observation');
    assert.equal(ok2.ok, true);
});

test('zod_validation_rejects_unknown_type_with_helpful_message', async () => {
    setBehavior({
        kind: 'ok',
        entries: FALLBACK_ANNOTATION_TYPES.map((name) => ({ name })),
    });
    await refreshAnnotationTypes();

    const result = await validateAnnotationType('bogus_type');
    assert.equal(result.ok, false);
    if (!result.ok) {
        // Lists the valid types
        assert.ok(
            result.error.includes('observation'),
            `error should list observation: ${result.error}`,
        );
        assert.ok(
            result.error.includes('correction'),
            `error should list correction: ${result.error}`,
        );
        assert.ok(
            result.error.includes('bogus_type'),
            'error should echo the bad value',
        );
        // Explains how to add a new type
        assert.ok(
            result.error.includes('publish a vocabulary_entry contribution'),
            'error should reference the contribution extension path',
        );
        assert.ok(
            result.error.includes('no code deploy'),
            'error should mention no-code-deploy as the point of the registry',
        );
        // validTypes field is populated
        assert.equal(result.validTypes.length, FALLBACK_ANNOTATION_TYPES.length);
    }
});

test('fallback_to_hardcoded_on_fetch_failure_preserves_genesis_types', async () => {
    setBehavior({ kind: 'status', status: 500 });
    const names = await refreshAnnotationTypes();
    // All 11 genesis entries survive the failure-mode fallback.
    assert.equal(names.length, FALLBACK_ANNOTATION_TYPES.length);
    for (const genesis of FALLBACK_ANNOTATION_TYPES) {
        assert.ok(
            names.includes(genesis),
            `fallback should include genesis type ${genesis}`,
        );
    }

    // And validation of a genesis type still succeeds
    const ok = await validateAnnotationType('correction');
    assert.equal(ok.ok, true);
});

test('help_text_renders_current_vocab', async () => {
    setBehavior({
        kind: 'ok',
        entries: [
            { name: 'observation' },
            { name: 'correction' },
            { name: 'custom_one' },
            { name: 'custom_two' },
        ],
    });
    await refreshAnnotationTypes();
    const types = getAnnotationTypesSync();
    assert.ok(types);
    const rendered = renderVocabTypeList(types!, '    ');
    // Custom types appear in the rendered help text
    assert.ok(rendered.includes('custom_one'), `should render custom_one: ${rendered}`);
    assert.ok(rendered.includes('custom_two'), `should render custom_two: ${rendered}`);
    assert.ok(rendered.includes('observation'));
});

test('async_getAnnotationTypes_returns_cached_set', async () => {
    setBehavior({
        kind: 'ok',
        entries: [{ name: 'a' }, { name: 'b' }],
    });
    await refreshAnnotationTypes();
    const names = await getAnnotationTypes();
    assert.deepEqual([...names], ['a', 'b']);
});
