const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const { runInNewContext } = require('node:vm');

function packageFactory(runtimeRoots) {
  const module = { exports: {} };
  runInNewContext(readFileSync(join(__dirname, '..', 'index.cjs'), 'utf8'), {
    __dirname: '/package',
    module,
    require(id) {
      if (id === './dist/kraai-runtime.node') {
        return { Runtime: { create: options => options } };
      }
      if (id === './dist/host.json') {
        return { executable: 'bundled-host', runtime_roots: runtimeRoots };
      }
      return require(id);
    },
  });
  return module.exports.createRuntime;
}

test('packaged host receives its declared runtime roots', () => {
  for (const roots of [[], ['/nix/store']]) {
    const options = packageFactory(roots)();
    assert.equal(options.nushell_host_path, '/package/dist/bundled-host');
    assert.deepEqual(Array.from(options.script_runtime_roots), roots);
  }
});

test('custom hosts do not inherit packaged roots and accept explicit roots', () => {
  const createRuntime = packageFactory(['/nix/store']);
  const custom = createRuntime({ nushell_host_path: '/custom/host' });
  assert.equal(custom.nushell_host_path, '/custom/host');
  assert.deepEqual(Array.from(custom.script_runtime_roots), []);
  const explicit = createRuntime({
    nushell_host_path: '/custom/host',
    script_runtime_roots: ['/custom/libraries'],
  });
  assert.deepEqual(Array.from(explicit.script_runtime_roots), ['/custom/libraries']);
  assert.deepEqual(Array.from(createRuntime({ script_runtime_roots: [] }).script_runtime_roots), []);
});

test('host roots cover Nix, conventional Linux, and static executables', async () => {
  const { hostRuntimeRoots } = await import('../scripts/host-roots.mjs');
  const headers = loader => `[Requesting program interpreter: ${loader}]`;
  assert.deepEqual(hostRuntimeRoots(headers('/nix/store/glibc/lib/ld-linux.so')), ['/nix/store']);
  const available = ['/lib', '/lib64', '/usr/lib', '/etc/ld.so.cache'];
  assert.deepEqual(
    hostRuntimeRoots(headers('/lib64/ld-linux-x86-64.so.2'), path => available.includes(path)),
    available,
  );
  assert.deepEqual(hostRuntimeRoots('No interpreter'), []);
  assert.throws(() => hostRuntimeRoots(headers('/custom/loader')), /Unsupported host dynamic loader/);
});
