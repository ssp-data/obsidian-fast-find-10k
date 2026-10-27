// Sets the plugin version in manifest.json, versions.json and plugin/package.json.
// Usage: node scripts/bump-version.mjs <x.y.z> [minAppVersion]   (run from the repo root)
import { readFileSync, writeFileSync } from 'node:fs';

const [version, minAppArg] = process.argv.slice(2);
if (!/^\d+\.\d+\.\d+$/.test(version ?? '')) {
	console.error('usage: node scripts/bump-version.mjs <x.y.z> [minAppVersion]');
	process.exit(1);
}
const read = (p) => JSON.parse(readFileSync(p, 'utf8'));
const write = (p, v) => writeFileSync(p, JSON.stringify(v, null, '\t') + '\n');

const manifest = read('manifest.json');
const minApp = minAppArg || manifest.minAppVersion;
manifest.version = version;
manifest.minAppVersion = minApp;
write('manifest.json', manifest);

const versions = read('versions.json');
versions[version] = minApp;
write('versions.json', versions);

const pkg = read('plugin/package.json');
pkg.version = version;
write('plugin/package.json', pkg);

const lock = read('plugin/package-lock.json');
lock.version = version;
if (lock.packages?.['']) lock.packages[''].version = version;
write('plugin/package-lock.json', lock);

console.log(`version ${version} (minAppVersion ${minApp}) written to manifest.json, versions.json, plugin/package.json, plugin/package-lock.json`);
