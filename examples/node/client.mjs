// Runs unchanged under Node.js and Bun. No hosted SDK or network connection is used.
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';

const binary = process.env.AETHER_MIDNIGHT_BIN ?? 'aether-midnight';
const database = process.env.AETHER_MIDNIGHT_DB;
const repoPath = process.env.AETHER_REPO ?? process.cwd();
const query = process.env.AETHER_QUERY ?? 'benchmark_target_00000';
const child = spawn(binary, database ? ['--db', database, 'mcp'] : ['mcp'], { stdio: ['pipe', 'pipe', 'inherit'] });
const lines = createInterface({ input: child.stdout });
let nextId = 1;
const pending = new Map();
lines.on('line', (line) => {
  const response = JSON.parse(line);
  const handlers = pending.get(response.id);
  if (handlers) {
    pending.delete(response.id);
    response.error ? handlers.reject(new Error(response.error.message)) : handlers.resolve(response.result);
  }
});

function request(method, params = {}) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
  });
}

try {
  await request('initialize', {
    protocolVersion: '2025-11-25',
    capabilities: {},
    clientInfo: { name: 'midnight-node-example', version: '1.0.0' },
  });
  child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' })}\n`);
  const result = await request('tools/call', {
    name: 'search_code',
    arguments: { repo_path: repoPath, query, limit: 5 },
  });
  const payload = JSON.parse(result.content[0].text);
  console.log(JSON.stringify(payload, null, 2));
} finally {
  child.stdin.end();
  await new Promise((resolve) => child.once('close', resolve));
}
