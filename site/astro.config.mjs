import { defineConfig } from 'astro/config';

export default defineConfig({
  site: process.env.SITE_URL ?? 'https://cc-loadout.pages.dev',
  output: 'static',
  build: { inlineStylesheets: 'auto' },
});
