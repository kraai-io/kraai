const assert = require('node:assert/strict');
const { mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');
const { test } = require('node:test');
const { createRuntime } = require('..');
const { localProvider, unwrap } = require('./fixtures.cjs');

for (const profile of ['coding', 'coding-no-sandbox']) {
  test(`packaged host executes ${profile} with explicit asset roots and isolated instructions`, { timeout: 30000 }, async t => {
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
    const session = unwrap(await runtime.createSessionWith({ workspace_dir: workspace, profile_id: profile }));
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
        assert.match(result.content.output.filter((part) => part.type === "text").map((part) => part.text).join("\n"), /42/);
        break;
      }
    }
    assert.ok(provider.requests[0].messages.some(message => message.content.includes('storage-root-only instruction')));
    events.close();
    unwrap(await runtime.shutdown());
  });
}

const fileContextCases = [false, true].flatMap(codex =>
  [false, true].map(symlinked => ({ codex, symlinked })));
for (const { codex, symlinked } of fileContextCases) {
  test(`opened files reach ${codex ? 'Codex' : 'Chat Completions'} HTTP requests after real commands${symlinked ? ' in a symlinked workspace' : ''}`, { timeout: 30000 }, async t => {
    const directory = mkdtempSync(join(tmpdir(), 'kraai-file-context-'));
    const workspace = join(directory, 'workspace');
    mkdirSync(workspace);
    writeFileSync(join(workspace, 'sample.txt'), 'original\r\nsecond line\n');
    const sessionWorkspace = symlinked ? join(directory, 'workspace-link') : workspace;
    if (symlinked) symlinkSync(workspace, sessionWorkspace, process.platform === 'win32' ? 'junction' : 'dir');
    const script = source => `<tool_call>\n# timeout=10sec\n${source}\n</tool_call>`;
    const provider = await localProvider(t, [
      script('kraai-open-files sample.txt'),
      script('42'),
      script('kraai-edit-file sample.txt [{start_line: 1, end_line: 1, old_text: "original\\r\\n", new_text: "updated\\r\\n"}]'),
      script('kraai-close-files sample.txt'),
      script('kraai-open-files sample.txt'),
      'Finished probe',
    ], { codex });
    const runtime = createRuntime({ storage_root: join(directory, 'storage') });
    t.after(async () => {
      await runtime.shutdown();
      rmSync(directory, { recursive: true, force: true });
    });
    assert.equal(unwrap(await runtime.waitForStartup()), 'Ready');
    unwrap(await runtime.saveSettings(provider.settings));
    const session = unwrap(await runtime.createSessionWith({ workspace_dir: sessionWorkspace, profile_id: 'coding-no-sandbox' }));
    const events = runtime.subscribe();
    t.after(() => events.close());
    unwrap(await runtime.sendMessage(session, 'Exercise the opened files', 'test-model', 'local'));
    for (;;) {
      const read = await events.next();
      assert.equal(read.type, 'event');
      const event = read.value.event;
      if (typeof event !== 'object') continue;
      for (const name of ['SessionError', 'StreamError', 'ContinuationFailed']) {
        assert.ok(!(name in event), JSON.stringify(event));
      }
      if ('ScriptResultReady' in event) assert.equal(event.ScriptResultReady.status, 'completed');
      if ('StreamComplete' in event && provider.requests.length === 6) break;
    }
    const messages = provider.requests.map(request => request.input ?? request.messages);
    const text = message => typeof message.content === 'string' ? message.content
      : (message.content ?? []).map(part => part.text ?? '').join('');
    const files = request => request.filter(message => text(message).startsWith('Opened file:'));
    assert.equal(files(messages[0]).length, 0);
    assert.equal(files(messages[1]).length, 1);
    assert.match(text(files(messages[1])[0]), /1\|original\r\n2\|second line\n/);
    assert.deepEqual(messages[2].slice(0, messages[1].length), messages[1]);
    assert.equal(files(messages[3]).length, 1);
    assert.match(text(messages[3].at(-1)), /1\|updated\r\n2\|second line\n/);
    assert.ok(!text(files(messages[3])[0]).includes('1|original'));
    assert.equal(files(messages[4]).length, 0);
    assert.equal(files(messages[5]).length, 1);
    assert.match(text(messages[5].at(-1)), /1\|updated\r\n/);
    const history = unwrap(await runtime.getChatHistory(session));
    assert.ok(!Object.values(history).some(message => message.content.type === 'file_context'));
    const context = JSON.parse(readFileSync(
      join(directory, 'storage', 'data', 'context-state', `${session}.json`), 'utf8'));
    const pins = context.events.flatMap(event => event.mutations)
      .filter(mutation => mutation.kind === 'pin-file');
    assert.equal(pins.length, 2);
    for (const pin of pins) {
      assert.equal(pin.scope.kind, 'workspace');
      assert.equal(realpathSync.native(pin.scope.root), realpathSync.native(workspace));
    }
    unwrap(await runtime.shutdown());
  });
}
