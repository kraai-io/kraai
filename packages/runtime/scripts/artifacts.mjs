export function runtimeArtifacts(cargoOutput) {
  const artifacts = cargoOutput.trim().split('\n')
    .filter(Boolean)
    .map(line => JSON.parse(line))
    .filter(message => message.reason === 'compiler-artifact');
  const library = artifacts.find(artifact => artifact.target.name === 'kraai_runtime_node'
    && artifact.target.crate_types.includes('cdylib'));
  const addon = library?.filenames.find(path => /\.(so|dylib|dll)$/.test(path));
  const host = artifacts.find(artifact => artifact.target.name === 'kraai-nushell-host')?.executable;
  const exporter = artifacts.find(artifact => artifact.target.name === 'export-types')?.executable;
  if (!addon || !host || !exporter) {
    throw new Error('Cargo did not produce the runtime addon, Nushell host, and type exporter');
  }
  return { addon, host, exporter };
}
