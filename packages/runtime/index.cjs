const { join } = require('node:path');
const { Runtime, EventSubscription } = require('./dist/kraai-runtime.node');
const host = require('./dist/host.json');

function createRuntime(options = {}) {
  const definedOptions = Object.fromEntries(
    Object.entries(options).filter(([, value]) => value !== undefined),
  );
  return Runtime.create({
    provider_config_path: null,
    storage_root: null,
    nushell_host_path: join(__dirname, 'dist', host.executable),
    script_runtime_roots: definedOptions.nushell_host_path === undefined ? host.runtime_roots : [],
    ...definedOptions,
  });
}

module.exports = { Runtime, EventSubscription, createRuntime };
