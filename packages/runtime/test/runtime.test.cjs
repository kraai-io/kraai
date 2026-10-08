const assert = require('node:assert/strict');
const { mkdtempSync, realpathSync, rmSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join, toNamespacedPath } = require('node:path');
const { test } = require('node:test');
const { localProvider, unwrap, readRecord } = require('./fixtures.cjs');
const { createRuntime } = require('..');

function fixture(t) {
  const directory = mkdtempSync(join(tmpdir(), 'kraai-node-test-'));
  return {
    directory,
    create(options = {}) {
      const runtime = createRuntime({ storage_root: directory, ...options });
      t.after(async () => {
        await runtime.shutdown();
        rmSync(directory, { recursive: true, force: true });
      });
      return runtime;
    },
  };
}

test('sessions, settings, structured errors, subscriptions and shutdown', { timeout: 30000 }, async (t) => {
  const { directory, create } = fixture(t);
  writeFileSync(join(directory, 'agents.toml'), '[[profiles]]\nid = "isolated"\nextends = "coding"\n');
  const runtime = create();
  assert.equal(unwrap(await runtime.waitForStartup()), 'Ready');
  const settings = unwrap(await runtime.getSettings());
  assert.deepEqual(settings, { providers: [], models: [] });
  unwrap(await runtime.saveSettings(settings));
  assert.ok(unwrap(await runtime.getAgentProfileCatalog(null)).profiles.some(profile => profile.id === 'isolated'));
  const id = unwrap(await runtime.createSessionWith({ workspace_dir: directory, profile_id: 'isolated' }));
  const snapshot = unwrap(await runtime.getSessionSnapshot(id));
  assert.equal(snapshot.session.id, id);
  assert.equal(snapshot.session.workspace_dir, toNamespacedPath(realpathSync.native(directory)));
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
  const { directory, create } = fixture(t);
  const config = join(directory, 'broken.toml');
  writeFileSync(config, 'not valid toml [');
  const runtime = create({ provider_config_path: config });
  const status = unwrap(await runtime.waitForStartup());
  assert.equal(typeof status.Failed, 'string');
  unwrap(await runtime.shutdown());
});

test('MCP auth methods return statuses and structured errors', { timeout: 30000 }, async t => {
  const runtime = fixture(t).create();
  assert.equal(unwrap(await runtime.waitForStartup()), 'Ready');
  assert.deepEqual(unwrap(await runtime.getMcpAuthStatuses()), []);
  for (const method of ['startMcpLogin', 'cancelMcpLogin', 'logoutMcp']) {
    const result = await runtime[method]('missing-server');
    assert.equal(result.Err.kind, 'not_found');
    assert.match(result.Err.message, /missing-server/);
  }
});

test('MCP auth distinguishes unsupported login from credential storage failures', { timeout: 30000 }, async t => {
  const { directory, create } = fixture(t);
  writeFileSync(join(directory, 'mcp.toml'), `
[servers.local]
transport = { type = "stdio", command = "unused-mcp-fixture" }
[servers.static]
transport = { type = "http", url = "http://mcp.internal/mcp", bearer_token_env = "UNUSED_MCP_TOKEN" }
[servers.disabled]
enabled = false
transport = { type = "stdio", command = "unused-mcp-fixture" }
[servers.oauth]
transport = { type = "http", url = "https://example.test/mcp" }
`);
  writeFileSync(join(directory, 'mcp-auth'), 'blocks credential directory creation');
  const runtime = create();
  assert.equal(unwrap(await runtime.waitForStartup()), 'Ready');
  for (const method of ['startMcpLogin', 'cancelMcpLogin', 'logoutMcp']) {
    assert.equal((await runtime[method]('disabled')).Err.kind, 'not_found');
    for (const server of ['local', 'static']) {
      const result = await runtime[method](server);
      assert.equal(result.Err.kind, 'invalid_argument');
      assert.match(result.Err.message, /OAuth login is available/);
    }
  }
  assert.equal((await runtime.logoutMcp('oauth')).Err.kind, 'internal');
});

test('streams a reply from a local provider and persists the conversation', { timeout: 30000 }, async (t) => {
  const { directory, create } = fixture(t);
  const provider = await localProvider(t, ['Hello from Rust']);
  const runtime = create();
  assert.equal(unwrap(await runtime.waitForStartup()), 'Ready');
  unwrap(await runtime.saveSettings(provider.settings));
  const session = unwrap(await runtime.createSessionWith({ workspace_dir: directory, profile_id: null }));
  const events = runtime.subscribe();
  const outcome = unwrap(await runtime.sendMessage(session, 'Hello', 'test-model', 'local', {}));
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
  const snapshot = unwrap(await runtime.getSessionSnapshot(session));
  const request = Object.values(snapshot.requests).find(value => value.model_id === 'test-model');
  assert.ok(request);
  const receipt = readRecord(directory, 'usage', request.message_id);
  assert.equal(receipt.model_id, 'test-model');
  events.close();
  unwrap(await runtime.shutdown());
});

test('native methods reject invalid and wrong-class receivers', { timeout: 30000 }, async t => {
  const { create } = fixture(t);
  const runtime = create();
  const events = runtime.subscribe();
  t.after(() => events.close());
  for (const [instance, wrongClass, syncMethod, asyncMethod] of [
    [runtime, events, 'startupStatus', 'listSessions'],
    [events, runtime, 'close', 'next'],
  ]) {
    for (const receiver of [null, undefined, {}, Object.create(Object.getPrototypeOf(instance)), wrongClass]) {
      assert.throws(() => instance[syncMethod].call(receiver));
      await assert.rejects(async () => instance[asyncMethod].call(receiver));
    }
  }
  assert.equal(unwrap(await runtime.waitForStartup()), 'Ready');
  events.close();
  assert.deepEqual(await events.next(), { type: 'closed' });
});

test('model options require explicit choices and survive request persistence', { timeout: 30000 }, async t => {
  const { directory, create } = fixture(t);
  const provider = await localProvider(t, ['Chosen options']);
  provider.settings.models[0].options = [{
    id: 'effort', label: 'Effort', type: 'choice', required: true,
    choices: [
      { id: 'low', label: 'Low', patch: { body: { reasoning_effort: 'low' }, headers: {} } },
      { id: 'high', label: 'High', patch: { body: { reasoning_effort: 'high' }, headers: {} } },
    ],
  }];
  const runtime = create();
  unwrap(await runtime.waitForStartup());
  unwrap(await runtime.saveSettings(provider.settings));
  const settings = unwrap(await runtime.getSettings());
  assert.deepEqual(settings.models[0].options, provider.settings.models[0].options);
  const session = unwrap(await runtime.createSessionWith({ workspace_dir: directory, profile_id: null }));
  const missing = await runtime.sendMessage(session, 'Hello', 'test-model', 'local', {});
  assert.ok('Err' in missing);
  assert.equal(provider.requests.length, 0);
  const events = runtime.subscribe();
  unwrap(await runtime.sendMessage(session, 'Hello', 'test-model', 'local', { effort: 'high' }));
  for (;;) {
    const read = await events.next();
    assert.equal(read.type, 'event');
    const event = read.value.event;
    if (typeof event === 'object' && 'StreamError' in event) assert.fail(JSON.stringify(event));
    if (typeof event === 'object' && 'StreamComplete' in event) break;
  }
  assert.equal(provider.requests[0].reasoning_effort, 'high');
  const snapshot = unwrap(await runtime.getSessionSnapshot(session));
  assert.deepEqual(snapshot.session.selected_model.options, { effort: 'high' });
  events.close();
});

test('session model options persist before a prompt and survive restart', { timeout: 30000 }, async t => {
  const directory = mkdtempSync(join(tmpdir(), 'kraai-node-model-options-'));
  let runtime;
  t.after(async () => {
    await runtime?.shutdown();
    rmSync(directory, { recursive: true, force: true });
  });
  const provider = await localProvider(t, []);
  provider.settings.models[0].options = [
    {
      id: 'effort', label: 'Effort', type: 'choice', required: true,
      binding: { type: 'body', path: '/reasoning_effort' },
      choices: [{ id: 'custom', label: 'Custom', patch: { body: {}, headers: {} } }],
    },
    { id: 'fast', label: 'Fast', type: 'boolean', required: true, binding: { type: 'body', path: '/fast' } },
    { id: 'budget', label: 'Budget', type: 'integer', min: 0, max: 100, binding: { type: 'body', path: '/custom_budget' } },
  ];
  runtime = createRuntime({ storage_root: directory });
  unwrap(await runtime.waitForStartup());
  unwrap(await runtime.saveSettings(provider.settings));
  const full = unwrap(await runtime.createSession());
  const partial = unwrap(await runtime.createSession());
  const selections = [
    [full, { provider_id: 'local', model_id: 'test-model', options: { effort: 'custom', fast: false, budget: 0 } }],
    [partial, { provider_id: 'local', model_id: 'test-model', options: { fast: true, budget: 50 } }],
  ];
  assert.equal(unwrap(await runtime.getSessionModel(full)), null);
  for (const [session, selection] of selections) {
    unwrap(await runtime.setSessionModel(session, selection));
    assert.deepEqual(unwrap(await runtime.getSessionModel(session)), selection);
  }
  const invalid = await runtime.setSessionModel(full, {
    ...selections[0][1], options: { effort: 'unsupported', fast: true },
  });
  assert.equal(invalid.Err.kind, 'invalid_argument');
  unwrap(await runtime.shutdown());
  runtime = createRuntime({ storage_root: directory });
  unwrap(await runtime.waitForStartup());
  for (const [session, selection] of selections) {
    assert.equal(unwrap(await runtime.loadSession(session)), true);
    assert.deepEqual(unwrap(await runtime.getSessionModel(session)), selection);
    const snapshot = unwrap(await runtime.getSessionSnapshot(session));
    assert.deepEqual(snapshot.session.selected_model, selection);
    assert.deepEqual(snapshot.history, {});
  }
  assert.equal(provider.requests.length, 0);
});

test('storage roots must be absolute and may be created at startup', { timeout: 30000 }, async t => {
  for (const storage_root of ['', '.', 'relative', '../relative']) {
    assert.throws(() => createRuntime({ storage_root }), /storage_root must be a non-empty absolute path/);
  }
  const { directory, create } = fixture(t);
  const storage_root = join(directory, 'new-storage');
  const runtime = create({ storage_root });
  assert.equal(unwrap(await runtime.waitForStartup()), 'Ready');
  assert.deepEqual(unwrap(await runtime.listSessions()), []);
});
