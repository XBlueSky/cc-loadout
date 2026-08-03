import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import sharp from 'sharp';

const sourcePath = fileURLToPath(new URL('../public/logo.png', import.meta.url));
const defaultOutputPath = fileURLToPath(new URL('../public/brand-mark.png', import.meta.url));
const outputPath = process.argv[2] ? resolve(process.argv[2]) : defaultOutputPath;

// The source sheet's lower-left app-mark specimen occupies this exact square.
// Pinning both Sharp and these coordinates makes regeneration reproducible.
await sharp(sourcePath)
  .extract({ left: 170, top: 885, width: 256, height: 256 })
  .png({ adaptiveFiltering: false, compressionLevel: 9 })
  .toFile(outputPath);
