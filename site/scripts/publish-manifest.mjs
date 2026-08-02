import { copyFile } from 'node:fs/promises';

const source = new URL('../../.cc-marketspec/dist/manifest.json', import.meta.url);
const destination = new URL('../dist/manifest.json', import.meta.url);
await copyFile(source, destination);
