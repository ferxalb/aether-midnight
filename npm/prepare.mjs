import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const [artifacts, destination, scope = '@SCOPE'] = process.argv.slice(2);
if (!artifacts || !destination || !/^@[a-zA-Z0-9_-]+$/.test(scope)) {
    throw new Error('Usage: node npm/prepare.mjs ARTIFACTS DESTINATION [@scope]');
}
const version = readFileSync('Cargo.toml', 'utf8').match(/^version = "([^"]+)"/m)[1];
const name = `${scope}/aether-midnight`;
const platforms = [
    ['darwin', 'x64', 'macos-universal2'],
    ['darwin', 'arm64', 'macos-universal2'],
    ['linux', 'x64', 'linux-x64-gnu'],
    ['linux', 'arm64', 'linux-arm64-gnu'],
    ['win32', 'x64', 'windows-x64'],
    ['win32', 'arm64', 'windows-arm64'],
];
const common = {
    version,
    license: 'Apache-2.0',
    repository: 'git+https://github.com/ferxalbs/aether-midnight.git',
};
const optionalDependencies = {};
const sha256 = data => createHash('sha256').update(data).digest('hex');
const packageDir = (slug, manifest) => {
    const path = resolve(destination, slug);
    mkdirSync(path, { recursive: true });
    copyFileSync('LICENSE', resolve(path, 'LICENSE'));
    writeFileSync(resolve(path, 'package.json'), JSON.stringify({ ...common, ...manifest }, null, 2) + '\n');
    return path;
};
for (const [os, cpu, release] of platforms) {
    const slug = `${os}-${cpu}${os === 'linux' ? '-gnu' : ''}`;
    const packageName = `${name}-${slug}`;
    optionalDependencies[packageName] = version;
    const releaseName = `aether-midnight-${version}-${release}`;
    const archive = resolve(artifacts, `${releaseName}.tar.gz`);
    const expected = readFileSync(`${archive}.sha256`, 'utf8').trim().split(/\s+/)[0];
    if (sha256(readFileSync(archive)) !== expected) throw new Error(`Archive checksum mismatch: ${release}`);
    const binaryName = os === 'win32' ? 'aether-midnight.exe' : 'aether-midnight';
    const extract = member => execFileSync('tar', ['-xOf', archive, `${releaseName}/${member}`], {
        maxBuffer: 256 * 1024 * 1024,
    });
    const binary = extract(`bin/${binaryName}`);
    const lines = extract('SHA256SUMS').toString().split(/\r?\n/);
    const entry = lines.map(line => line.trim().split(/\s+/)).find(([, path]) => path === `./bin/${binaryName}`);
    if (!entry || sha256(binary) !== entry[0]) throw new Error(`Binary checksum mismatch: ${release}`);
    const pkg = packageDir(slug, { name: packageName, os: [os], cpu: [cpu],
        ...(os === 'linux' ? { libc: ['glibc'] } : {}), files: ['bin'],
        binarySha256: entry[0] });
    mkdirSync(resolve(pkg, 'bin'), { recursive: true });
    writeFileSync(resolve(pkg, 'bin', binaryName), binary, { mode: 0o755 });
}
const main = packageDir('main', {
    name, optionalDependencies, files: ['midnight.cjs'],
    bin: { midnight: 'midnight.cjs', 'aether-midnight': 'midnight.cjs' },
});
copyFileSync('npm/midnight.cjs', resolve(main, 'midnight.cjs'));
