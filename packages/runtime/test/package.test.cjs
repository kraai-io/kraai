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
    assert.equal(options.nushell_host_path, join('/package', 'dist', 'bundled-host'));
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

test('undefined options preserve defaults without mutating the caller', () => {
  const createRuntime = packageFactory(['/nix/store']);
  for (const input of [
    { nushell_host_path: undefined },
    { script_runtime_roots: undefined },
    { nushell_host_path: undefined, script_runtime_roots: undefined },
  ]) {
    const options = createRuntime(Object.freeze(input));
    assert.equal(options.nushell_host_path, join('/package', 'dist', 'bundled-host'));
    assert.deepEqual(Array.from(options.script_runtime_roots), ['/nix/store']);
  }
  const custom = createRuntime({ nushell_host_path: '/custom/host', script_runtime_roots: undefined });
  assert.deepEqual(Array.from(custom.script_runtime_roots), []);
  const defaults = createRuntime({ storage_root: undefined, provider_config_path: undefined });
  assert.equal(defaults.storage_root, null);
  assert.equal(defaults.provider_config_path, null);
  assert.equal(createRuntime({ script_runtime_roots: null }).script_runtime_roots, null);
});

test('build artifacts use Cargo paths on Linux, macOS and Windows', async () => {
  const { runtimeArtifacts } = await import('../scripts/artifacts.mjs');
  for (const [addon, host, exporter] of [
    ['/target/libkraai_runtime_node.so', '/target/kraai-nushell-host', '/target/export-types'],
    ['/target/libkraai_runtime_node.dylib', '/target/kraai-nushell-host', '/target/export-types'],
    ['C:\\target\\kraai_runtime_node.dll', 'C:\\target\\kraai-nushell-host.exe', 'C:\\target\\export-types.exe'],
  ]) {
    const output = [
      { reason: 'build-script-executed' },
      { reason: 'compiler-artifact', target: { name: 'kraai_runtime_node', crate_types: ['cdylib', 'rlib'] }, filenames: [addon + '.lib', addon] },
      { reason: 'compiler-artifact', target: { name: 'kraai-nushell-host' }, executable: host },
      { reason: 'compiler-artifact', target: { name: 'export-types' }, executable: exporter },
    ].map(value => JSON.stringify(value)).join('\n');
    assert.deepEqual(runtimeArtifacts(output), { addon, host, exporter });
  }
  assert.throws(() => runtimeArtifacts(''), /Cargo did not produce/);
});

test('macOS hosts include dynamic library roots and Nix transitive dependencies', async () => {
  const { darwinRuntimeRoots } = await import('../scripts/host-roots.mjs');
  const libraries = paths => ['/target/host:', ...paths.map(path => `\t${path} (compatibility version 1.0.0, current version 1.0.0)`)].join('\n');
  assert.deepEqual(darwinRuntimeRoots(libraries(['/usr/lib/libSystem.B.dylib'])), ['/usr/lib']);
  assert.deepEqual(darwinRuntimeRoots(libraries([
    '/nix/store/openssl/lib/libssl.dylib', '/nix/store/openssl/lib/libcrypto.dylib',
    '/usr/lib/libSystem.B.dylib',
  ])), ['/nix/store', '/usr/lib']);
  assert.throws(() => darwinRuntimeRoots(libraries(['@rpath/libcustom.dylib'])), /Unsupported host library path/);
});
