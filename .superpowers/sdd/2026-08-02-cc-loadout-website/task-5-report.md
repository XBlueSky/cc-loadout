# Task 5 Report: Publish the Manifest and Complete Metadata

## Outcome

- Added `site/scripts/publish-manifest.mjs`; the site build now copies `.cc-marketspec/dist/manifest.json` to `site/dist/manifest.json` after Astro finishes.
- Added Open Graph and X/Twitter metadata, including absolute social-image URLs derived from `Astro.site` with the `https://cc-loadout.pages.dev` fallback.
- Kept the existing PNG favicon link (`/logo.png`) and staged the accepted social card at `site/public/og.png`.
- Added build-artifact coverage that verifies the published manifest deep-equals the generated manifest and the built page exposes the required metadata.

## TDD record

1. Added `site/test/build.test.mjs` before production changes.
2. Ran `npm run site:build && node --test site/test/build.test.mjs`.
   It failed as expected: `site/dist/manifest.json` was absent and the page had no `twitter:card` metadata.
3. Implemented the post-build publication script and social metadata.
4. Re-ran the build tests successfully, then completed the full verification below.

## Social-card provenance and validation

Creative prompt used by the task controller (verbatim):

> Create a 1200×630 social preview for the finished cc-loadout website. Preserve the supplied logo design and spelling. Use a charcoal-black field, warm-white technical linework, and equipment-orange accents. Compose the cc-loadout toolbox mark as the hero, with the exact text “cc-loadout” and “One plugin. Every loadout.” Use premium editorial developer-tool art direction, restrained mechanical grid details, generous negative space, high legibility at thumbnail size, and no additional logos, badges, fake UI, or invented product claims.

Tool provenance: the controller used the `imagegen` skill exactly twice (the original attempt and its one permitted retry), accepted the final asset, and placed it at `site/public/og.png`. This task did not call image generation or transform the accepted file.

Asset validation performed without modifying the file:

- `file`: PNG, 8-bit RGB, non-interlaced
- `sips`: 1200 × 630 pixels
- SHA-256: `9dd7a5a6ab7289477b29c5476e4f9c8114fc61a54d102ed0b246d3de029858cc`
- Visual inspection: the artwork retains the exact `cc-loadout` name and `One plugin. Every loadout.` tagline, with no invented product claims.

## Verification

- `npm run site:build && node --test site/test/build.test.mjs` — 2 passing
- `npm run site:verify` — 19 passing site tests; manifest contract test passing
- `npm run manifest:check` — passed
- `git diff --check` — passed

The generated `site/dist/manifest.json` is intentionally not staged; the build test confirms it deep-equals `.cc-marketspec/dist/manifest.json`.

## Final-review fix round 1

- Finding addressed: the page had an absolute social image but no canonical URL or `og:url`, leaving custom-domain publication incomplete.
- RED: added a real isolated Astro build with `SITE_URL=https://custom.example.test`; `npm run site:build && node --test site/test/build.test.mjs` failed because the built page lacked `<link rel="canonical">`.
- GREEN: derived the page URL from `Astro.url.pathname` and `Astro.site`, retaining `https://cc-loadout.pages.dev` when no site is configured, and emitted matching absolute canonical and `og:url` values. The same command then passed all 3 build-artifact tests, including exact custom-domain values and a guard against localhost output.
- The existing social-image URL behavior and accepted `site/public/og.png` were left unchanged.
