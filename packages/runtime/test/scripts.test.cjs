const assert = require('node:assert/strict');
const { mkdtempSync, mkdirSync, rmSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');
const { test } = require('node:test');
const { createRuntime } = require('..');
const { localProvider, unwrap } = require('./fixtures.cjs');

test('packaged host executes with explicit asset roots and isolated instructions', { timeout: 30000 }, async t => {
  const directory = mkdtempSync(join(tmpdir(), 'kraai-node-script-'));
  const storage = join(directory, 'storage');
  const workspace = join(directory, 'workspace');
  mkdirSync(storage);
  mkdirSync(workspace);
  writeFileSync(join(storage, 'AGENTS.md'), 'Use the storage-root-only instruction.');
  const provider = await localProvider(t, [
    '<tool_call>\n# timeout=10sec permissions=workspace-read\n42\n</tool_call>',
    'Completed',
  ]);
  const runtime = createRuntime({ storage_root: storage });
  t.after(async () => {
    await runtime.shutdown();
    rmSync(directory, { recursive: true, force: true });
  });
  assert.equal(unwrap(await runtime.waitForStartup()), 'Ready');
  unwrap(await runtime.saveSettings(provider.settings));
  const session = unwrap(await runtime.createSessionWith({ workspace_dir: workspace, profile_id: 'plan' }));
  const events = runtime.subscribe();
  unwrap(await runtime.sendMessage(session, 'Calculate', 'test-model', 'local'));
  for (;;) {
    const read = await events.next();
    assert.equal(read.type, 'event');
    const event = read.value.event;
    if (typeof event !== 'object') continue;
    assert.ok(!('SessionError' in event) && !('StreamError' in event), JSON.stringify(event));
    if ('ScriptResultReady' in event) {
      const history = unwrap(await runtime.getChatHistory(session));
      const result = Object.values(history).find(message => message.content.type === 'script_result');
      assert.equal(event.ScriptResultReady.status, 'completed', JSON.stringify(result));
      assert.match(result.content.output, /42/);
      break;
    }
  }
  assert.ok(provider.requests[0].messages.some(message => message.content.includes('storage-root-only instruction')));
  events.close();
  unwrap(await runtime.shutdown());
});
