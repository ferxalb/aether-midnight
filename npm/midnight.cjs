#!/usr/bin/env node
'use strict';

const { spawn } = require('node:child_process');
const { name } = require('./package.json');
const libc = process.platform === 'linux' ? '-gnu' : '';
if (libc && !process.report.getReport().header.glibcVersionRuntime) {
    console.error('Midnight requires glibc on Linux.');
    process.exit(1);
}
const binaryName = process.platform === 'win32' ? 'aether-midnight.exe' : 'aether-midnight';
const platformPackage = `${name}-${process.platform}-${process.arch}${libc}`;
let binary;
try {
    binary = require.resolve(`${platformPackage}/bin/${binaryName}`);
} catch {
    console.error(`Midnight could not locate ${platformPackage}. Install with optional dependencies enabled.`);
    process.exit(1);
}
const child = spawn(binary, process.argv.slice(2), {
    stdio: 'inherit',
    env: { ...process.env, MIDNIGHT_NPM_MANAGED: '1' },
});
const forward = signal => child.kill(signal);
const signals = ['SIGINT', 'SIGTERM'];
const handlers = signals.map(signal => () => forward(signal));
signals.forEach((signal, index) => process.on(signal, handlers[index]));
child.on('error', error => {
    console.error(error.message);
    process.exitCode = 1;
});
child.on('exit', (code, signal) => {
    signals.forEach((name, index) => process.removeListener(name, handlers[index]));
    if (signal) process.kill(process.pid, signal);
    else process.exitCode = code ?? 1;
});
