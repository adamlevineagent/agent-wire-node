import { createServer } from 'node:http';
import { AddressInfo } from 'node:net';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test, { after } from 'node:test';
import assert from 'node:assert/strict';

let seenBody: unknown = null;
let seenAuth: string | undefined;

const server = createServer((req, res) => {
  if (req.method !== 'POST' || req.url !== '/api/v1/pyramid/vocabulary') {
    res.statusCode = 404;
    res.end();
    return;
  }
  seenAuth = req.headers.authorization;
  const chunks: Buffer[] = [];
  req.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
  req.on('end', () => {
    seenBody = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    res.statusCode = 200;
    res.setHeader('content-type', 'application/json');
    res.end(
      JSON.stringify({
        contribution_id: 'cid-cli-test',
        vocab_kind: 'annotation_type',
        name: 'cli_publish_type',
        entry: {
          name: 'cli_publish_type',
          description: 'CLI-published annotation type',
          handler_chain_id: 'starter-debate-steward',
          reactive: true,
          creates_delta: false,
          include_in_cascade_prompt: true,
        },
      }),
    );
  });
});

const baseUrl = await new Promise<string>((resolve, reject) => {
  server.once('error', reject);
  server.listen(0, '127.0.0.1', () => {
    const addr = server.address() as AddressInfo;
    resolve(`http://127.0.0.1:${addr.port}`);
  });
});

after(async () => {
  await new Promise<void>((resolve) => server.close(() => resolve()));
});

function runCli(args: string[]): Promise<{ code: number | null; stdout: string; stderr: string }> {
  const cliPath = fileURLToPath(new URL('../src/cli.js', import.meta.url));
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [cliPath, ...args], {
      env: {
        ...process.env,
        PYRAMID_MCP_BASE_URL: baseUrl,
        PYRAMID_AUTH_TOKEN: 'cli-test-token',
      },
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => {
      stdout += chunk.toString();
    });
    child.stderr.on('data', (chunk) => {
      stderr += chunk.toString();
    });
    child.on('close', (code) => resolve({ code, stdout, stderr }));
  });
}

test('vocab_publish_cli_posts_to_api_v1_surface_and_prints_contribution_id', async () => {
  const result = await runCli([
    'vocab',
    'publish',
    '--kind',
    'annotation_type',
    '--name',
    'cli_publish_type',
    '--description',
    'CLI-published annotation type',
    '--handler-chain-id',
    'starter-debate-steward',
    '--reactive',
    'true',
    '--creates-delta',
    'false',
    '--include-in-cascade-prompt',
    'true',
    '--compact',
  ]);

  assert.equal(result.code, 0, result.stderr);
  const output = JSON.parse(result.stdout);
  assert.equal(output.contribution_id, 'cid-cli-test');
  assert.equal(seenAuth, 'Bearer cli-test-token');
  assert.deepEqual(seenBody, {
    vocab_kind: 'annotation_type',
    name: 'cli_publish_type',
    description: 'CLI-published annotation type',
    handler_chain_id: 'starter-debate-steward',
    reactive: true,
    creates_delta: false,
    include_in_cascade_prompt: true,
  });
});

test('vocab_publish_cli_rejects_alias_ambiguity_before_posting', async () => {
  const cases = [
    {
      args: [
        '--kind',
        'annotation_type',
        '--type',
        'role_name',
        '--name',
        'cli_alias_kind',
        '--description',
        'kind aliases should conflict',
      ],
      error: 'use only one of --kind, --vocab-kind, --type',
    },
    {
      args: [
        '--kind',
        'annotation_type',
        '--name',
        'cli_alias_name',
        '--term',
        'cli_alias_term',
        '--description',
        'name aliases should conflict',
      ],
      error: 'use only one of --name, --term',
    },
    {
      args: [
        '--kind',
        'annotation_type',
        '--name',
        'cli_alias_description',
        '--description',
        'description aliases should conflict',
        '--definition',
        'definition aliases should conflict',
      ],
      error: 'use only one of --description, --definition',
    },
  ];

  for (const c of cases) {
    seenBody = null;
    const result = await runCli(['vocab', 'publish', ...c.args]);
    assert.equal(result.code, 1, result.stderr);
    assert.match(result.stderr, new RegExp(c.error.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
    assert.equal(seenBody, null, 'CLI should reject alias ambiguity before HTTP POST');
  }
});
