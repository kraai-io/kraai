const assert = require('node:assert/strict');
const { mkdtempSync, rmSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');
const { test } = require('node:test');
const { localProvider, unwrap } = require('./fixtures.cjs');
const { createRuntime } = require('..');

function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'kraai-node-test-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}

test('sessions, settings, structured errors, subscriptions and shutdown', { timeout: 30000 }, async (t) => {
  const directory = fixture(t);
  writeFileSync(join(directory, 'agents.toml'), '[[profiles]]\nid = "isolated"\nextends = "plan"\n');
  const runtime = createRuntime({ storage_root: directory });
  t.after(() => runtime.shutdown());
  assert.equal(unwrap(await runtime.waitForStartup()), 'Ready');
  const settings = unwrap(await runtime.getSettings());
  assert.deepEqual(settings, { providers: [], models: [] });
  unwrap(await runtime.saveSettings(settings));
  assert.ok(unwrap(await runtime.getAgentProfileCatalog(null)).profiles.some(profile => profile.id === 'isolated'));
  const id = unwrap(await runtime.createSessionWith({ workspace_dir: directory, profile_id: 'isolated' }));
  const snapshot = unwrap(await runtime.getSessionSnapshot(id));
  assert.equal(snapshot.session.id, id);
  assert.equal(snapshot.session.workspace_dir, directory);
  assert.equal(snapshot.activity, 'idle');
  assert.equal(snapshot.profiles.selected_profile_id, 'isolated');
  assert.ok(snapshot.profiles.profiles.some(profile => profile.id === 'isolated'));
  assert.equal(typeof snapshot.event_sequence, 'number');
  assert.ok(Array.isArray(snapshot.profiles.profiles[0].capabilities));
  assert.ok(unwrap(await runtime.listSessions()).some(session => session.id === id));
  assert.equal((await runtime.getSessionSnapshot('missing')).Err.kind, 'not_found');
  assert.equal((await runtime.listUserInputHistory(-1)).Err.kind, 'invalid_argument');
  assert.equal((await runtime.listUserInputHistory(Number.MAX_SAFE_INTEGER + 1)).Err.kind, 'invalid_argument');
  assert.deepEqual(unwrap(await runtime.listUserInputHistory(2 ** 32)), []);
  assert.deepEqual(unwrap(await runtime.listUserInputHistory(Number.MAX_SAFE_INTEGER)), []);
  const events = runtime.subscribe();
  const pending = events.next();
  events.close();
  assert.deepEqual(await pending, { type: 'closed' });
  const second = runtime.subscribe();
  const read = second.next();
  unwrap(await runtime.shutdown());
  assert.deepEqual(await read, { type: 'closed' });
  unwrap(await runtime.shutdown());
  assert.equal((await runtime.listSessions()).Err.kind, 'unavailable');
});

test('startup failures remain observable and can be shut down', { timeout: 30000 }, async (t) => {
  const directory = fixture(t);
  const config = join(directory, 'broken.toml');
  writeFileSync(config, 'not valid toml [');
  const runtime = createRuntime({ storage_root: directory, provider_config_path: config });
  t.after(() => runtime.shutdown());
  const status = unwrap(await runtime.waitForStartup());
  assert.equal(typeof status.Failed, 'string');
  unwrap(await runtime.shutdown());
});

test('streams a reply from a local provider and persists the conversation', { timeout: 30000 }, async (t) => {
  const directory = fixture(t);
  const provider = await localProvider(t, ['Hello from Rust']);
  const runtime = createRuntime({ storage_root: directory });
  t.after(() => runtime.shutdown());
  assert.equal(unwrap(await runtime.waitForStartup()), 'Ready');
  unwrap(await runtime.saveSettings(provider.settings));
  const session = unwrap(await runtime.createSessionWith({ workspace_dir: directory, profile_id: null }));
  const events = runtime.subscribe();
  const outcome = unwrap(await runtime.sendMessage(session, 'Hello', 'test-model', 'local'));
  assert.equal(outcome.disposition, 'started');
  let chunks = '';
  let sequence = 0;
  for (;;) {
    const read = await events.next();
    assert.equal(read.type, 'event');
    assert.ok(read.value.sequence > sequence);
    sequence = read.value.sequence;
    const event = read.value.event;
    if (typeof event !== 'object') continue;
    assert.ok(!('StreamError' in event), JSON.stringify(event));
    if ('StreamChunk' in event) chunks += event.StreamChunk.chunk;
    if ('StreamComplete' in event) break;
  }
  assert.equal(chunks, 'Hello from Rust');
  const history = unwrap(await runtime.getChatHistory(session));
  assert.ok(Object.values(history).some(message => message.content.type === 'assistant'));
  events.close();
  unwrap(await runtime.shutdown());
});
