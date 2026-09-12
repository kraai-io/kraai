const { join } = require('node:path');
const { Runtime, EventSubscription } = require('./dist/kraai-runtime.node');
const host = require('./dist/host.json');

function createRuntime(options = {}) {
  return Runtime.create({
    provider_config_path: null,
    storage_root: null,
    nushell_host_path: join(__dirname, 'dist', host.executable),
    script_runtime_roots: options.nushell_host_path === undefined ? host.runtime_roots : [],
    ...options,
  });
}

module.exports = { Runtime, EventSubscription, createRuntime };
