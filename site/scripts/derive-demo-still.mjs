import { resolve } from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
import sharp from 'sharp';

export const DEMO_FRAME_RATE = 25;
export const DEMO_STILL_FRAME = 225;

const sourcePath = fileURLToPath(new URL('../public/demo.gif', import.meta.url));
const defaultOutputPath = fileURLToPath(new URL('../public/demo-still.png', import.meta.url));

// Frame 225 / 9.00s is a settled Profile → Rules view: it shows the frontend
// profile, one matched repository, and three actionable near-miss suggestions.
export async function deriveDemoStill(outputPath = defaultOutputPath) {
  await sharp(sourcePath, { page: DEMO_STILL_FRAME })
    .png({ adaptiveFiltering: false, compressionLevel: 9 })
    .toFile(outputPath);
}

const invokedPath = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : null;
if (invokedPath === import.meta.url) {
  await deriveDemoStill(process.argv[2] ? resolve(process.argv[2]) : defaultOutputPath);
}
