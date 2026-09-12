const assert = require('node:assert/strict');
const { createServer } = require('node:http');
const { once } = require('node:events');

async function localProvider(t, replies) {
  const requests = [];
  const server = createServer((request, response) => {
    let body = '';
    request.on('data', chunk => { body += chunk; });
    request.on('end', () => {
      if (request.url === '/models') {
        response.setHeader('Content-Type', 'application/json');
        response.end(JSON.stringify({ data: [{ id: 'test-model' }] }));
      } else {
        const content = replies[requests.length] ?? 'Done';
        requests.push(JSON.parse(body));
        response.setHeader('Content-Type', 'text/event-stream');
        response.end(`data: ${JSON.stringify({ choices: [{ delta: { content } }] })}\n\ndata: [DONE]\n\n`);
      }
    });
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  t.after(() => new Promise(resolve => server.close(resolve)));
  return {
    requests,
    settings: {
      providers: [{ id: 'local', type_id: 'openai-chat-completions', values: [
        { key: 'base_url', value: `http://127.0.0.1:${server.address().port}` },
        { key: 'api_key', value: 'test' },
      ] }],
      models: [{ id: 'test-model', provider_id: 'local', values: [] }],
    },
  };
}

function unwrap(result) {
  assert.ok('Ok' in result, JSON.stringify(result));
  return result.Ok;
}

module.exports = { localProvider, unwrap };
