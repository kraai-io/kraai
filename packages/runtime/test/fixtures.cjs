const assert = require('node:assert/strict');
const { createServer } = require('node:http');
const { once } = require('node:events');

async function localProvider(t, replies, { codex = false } = {}) {
  const tokenEnv = 'KRAAI_TEST_CODEX_PROXY_TOKEN';
  if (codex) {
    const previous = process.env[tokenEnv];
    process.env[tokenEnv] = 'local-test-token';
    t.after(() => {
      if (previous === undefined) delete process.env[tokenEnv];
      else process.env[tokenEnv] = previous;
    });
  }
  const requests = [];
  const server = createServer((request, response) => {
    let body = '';
    request.on('data', chunk => { body += chunk; });
    request.on('end', () => {
      if (request.url === '/models' || request.url.startsWith('/codex/models?')) {
        response.setHeader('Content-Type', 'application/json');
        response.end(JSON.stringify(codex ? { models: [{
          slug: 'test-model', display_name: 'Test Model', visibility: 'list',
          supported_reasoning_levels: [],
        }] } : { data: [{ id: 'test-model' }] }));
      } else {
        const index = requests.length;
        const reply = replies[index] ?? 'Done';
        const toolCall = typeof reply === 'object';
        requests.push(JSON.parse(body));
        response.setHeader('Content-Type', 'text/event-stream');
        if (codex) {
          const event = toolCall ? {
            type: 'response.output_item.done',
            item: { type: 'custom_tool_call', call_id: `call-${index}`, name: 'kraai_nushell', input: reply.input },
          } : { type: 'response.output_text.delta', item_id: `message-${index}`, delta: reply };
          response.end([event, { type: 'response.completed', response: { usage: { input_tokens: 100, output_tokens: 10 } } }]
            .map(value => `data: ${JSON.stringify(value)}\n\n`).join(''));
        } else {
          const delta = toolCall ? { tool_calls: [{ index: 0, id: `call-${index}`, type: 'function', function: { name: 'kraai_nushell', arguments: JSON.stringify(reply) } }] } : { content: reply };
          response.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: toolCall ? 'tool_calls' : 'stop' }] })}\n\ndata: [DONE]\n\n`);
        }
      }
    });
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise(resolve => server.close(resolve)));
  return {
    requests,
    settings: {
      providers: [{ id: 'local', type_id: codex ? 'openai-codex' : 'openai-chat-completions', values: [
        { key: 'base_url', value: `http://127.0.0.1:${server.address().port}` },
        codex ? { key: 'proxy_token_env', value: tokenEnv } : { key: 'api_key', value: 'test' },
      ] }],
      models: [{ id: 'test-model', provider_id: 'local', values: [] }],
    },
  };
}

function unwrap(result) {
  assert.ok('Ok' in result, JSON.stringify(result));
  return result.Ok;
}

function readRecord(storage, kind, id) {
  const { DatabaseSync } = require('node:sqlite');
  const { join } = require('node:path');
  const database = new DatabaseSync(join(storage, 'data', 'kraai.sqlite3'), { readOnly: true });
  try {
    return JSON.parse(database.prepare('SELECT data FROM records WHERE kind = ? AND id = ?').get(kind, id).data);
  } finally {
    database.close();
  }
}

module.exports = { localProvider, unwrap, readRecord };
