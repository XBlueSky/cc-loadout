# cc-loadout Website Design

**Date:** 2026-08-02
**Status:** Approved in conversation; awaiting written-spec review

## Summary

Build an English-first product website for cc-loadout that presents the Claude Code plugin as the recommended user experience and the Rust CLI/TUI as its execution and visual-configuration layer. The site will use an Apple-like cinematic scroll narrative without copying Apple's visual language. Its own identity comes from the existing cc-loadout logo: charcoal black, warm white, equipment orange, monospaced operational details, and a precise editorial layout.

The site will be a static Astro project under `site/`. It will generate and validate marketplace presentation data with `@xbluesky/cc-marketspec` before every build, consume the generated manifest at build time, and publish that same manifest at `/manifest.json`. GitHub Actions will validate pull requests and pushes. Cloudflare Pages Git integration will own deployment.

## Goals

- Make the Claude Code plugin the primary product entry point and recommended workflow.
- Explain honestly that the plugin depends on the separately installed cc-loadout binary.
- Position the TUI as the visual way to inspect and configure loadouts, not as a competing product.
- Explain the core product value: the right plugins and account for each repository, without loading every plugin everywhere.
- Create a premium, scroll-driven product story with polished desktop motion and a simplified mobile experience.
- Use cc-marketspec authored data as the website's structured product data source.
- Keep generated marketplace data out of Git history.
- Publish a stable manifest endpoint together with the website.
- Validate the complete data-to-site pipeline in GitHub Actions before deployment.

## Non-goals

- Building a documentation portal or multi-page application.
- Adding a backend, runtime database, authentication, analytics, or a runtime manifest API.
- Re-implementing cc-loadout features in the browser.
- Creating a 3D/WebGL product film or scroll-jacking experience.
- Redesigning the existing logo.
- Claiming that the Claude Code plugin works without the cc-loadout binary.
- Committing `.cc-marketspec/dist/manifest.json` or having CI write generated files back to the repository.

## Product Positioning

The homepage presents one product with three layers:

1. **Claude Code plugin — recommended experience.** Claude guides setup and invokes supported workflows through the bundled skills.
2. **Rust CLI — execution engine.** It performs discovery, validation, atomic writes, account operations, profile application, and scheduled tasks.
3. **TUI — visual control surface.** It makes profiles, rules, assignments, drift, and account state visible and editable.

The installation section remains technically honest. It describes a two-step setup: install the binary, then add the marketplace and install the plugin. The plugin is the narrative and visual focus even though the binary prerequisite must be completed first.

## Information Architecture

The website is a single English page with four navigation targets:

- Product
- How it works
- Install
- GitHub

The page has five scroll-story scenes and a compact footer.

### Scene 1: Hero — “One plugin. Every loadout.”

The cc-loadout mark appears on a charcoal field. As the visitor scrolls, the toolbox concept separates into terminal, plugin, and profile layers. The supporting copy establishes the product hierarchy: Claude guides setup, the TUI makes it visible, and the Rust CLI executes changes safely.

The primary action scrolls to installation. A secondary action opens GitHub.

### Scene 2: Problem — “Your tools shouldn’t follow you everywhere.”

Plugin cards accumulate until they crowd the scene. Continued scrolling filters them down to only the tools needed by the active repository. This explains plugin budget pressure and skill misfires without inventing numeric benchmarks.

### Scene 3: Profiles — “Every repo gets the right kit.”

Repository types change while plugin cards reorganize into Universal, Profile, and On-demand groups. The animation maps to real `detect`, `apply`, and on-demand behavior. Copy and labels must come from supported cc-loadout concepts.

### Scene 4: Visual control — “Claude guides it. The TUI makes it visible.”

The real TUI becomes the dominant visual. It shows profile assignment, detection rules, near-miss feedback, and account switching. The implementation will use the existing `docs/assets/demo.gif` or clear frames derived from it. It will not create a fake interface that implies unsupported behavior.

### Scene 5: Install — “Ready your loadout.”

The final scene presents two concrete steps:

1. Install the Rust CLI engine.
2. Add the cc-loadout marketplace and install the Claude Code plugin.

Commands are copyable and keyboard accessible. The section also links to the README for platform requirements and limitations.

### Footer

The footer links to GitHub, documentation, the MIT license, and `/manifest.json`. It avoids a large sitemap because the first release is intentionally a focused product page.

## Visual System

### Direction

The selected direction is **Loadout Console**: a dark editorial product page that uses terminal output and real product visuals as evidence. It should feel like a premium developer tool—precise, restrained, mechanical, and trustworthy—rather than a generic SaaS landing page or decorative cyberpunk interface.

### Palette

- Charcoal black for primary surfaces.
- Warm white for primary copy and logo linework.
- Equipment orange for active states, commands, progress, and key phrases.
- Neutral warm grays for structure and secondary text.

Colors will be sampled from the supplied `logo.png` during implementation and encoded as reusable design tokens.

### Typography

- Space Grotesk or an equivalent geometric display face for product headlines.
- Geist Mono for commands, labels, numbering, and operational status.
- Fonts will be self-hosted so the deployed page does not depend on a third-party font request.

### Layout and texture

- A responsive twelve-column desktop grid with generous negative space.
- Large editorial typography paired with small monospaced operational labels.
- Thin rules, squared geometry, and minimal radius.
- A restrained noise/grid texture to keep dark areas tactile without reducing readability.
- No decorative pill collections, excessive glass effects, or repetitive rounded feature cards.

### Logo usage

The supplied `logo.png` remains the source asset. Implementation may produce optimized crops for the mark and horizontal lockup, but will not redraw or reinterpret the logo. The original asset remains intact.

## Motion System

Astro produces the complete static document. GSAP ScrollTrigger progressively enhances it with cinematic scrolling.

### Motion principles

- Native scrolling is preserved; there is no scroll hijacking.
- Sticky scenes use scroll progress to control transforms, opacity, clipping, and layer changes.
- Each scene communicates one product idea and includes a visual pause before transition.
- Motion emphasizes product relationships rather than adding continuous decorative movement.
- Animations favor transform and opacity to reduce layout and paint cost.
- Expensive blur and large compositing layers are limited.
- Motion initializes only after the static content is available.

### Responsive behavior

- Desktop receives the complete pinned-scene choreography.
- Tablet reduces travel distance and simultaneous layers.
- Mobile keeps the same story order but shortens or removes pinned durations and uses simpler transitions.
- Content remains in document order at every breakpoint.

### Reduced motion and no-JavaScript behavior

When `prefers-reduced-motion: reduce` is active, each scene renders directly in a readable completed state. Without JavaScript, all copy, product imagery, installation commands, links, and manifest-derived data remain visible and usable.

## Data Architecture

Authored marketplace presentation data lives in:

- `.cc-marketspec/catalog.yaml`
- `.cc-marketspec/entries/plugin-cc-loadout.yaml`

The native source remains:

- `.claude-plugin/marketplace.json`
- `.claude-plugin/plugin.json`
- Bundled skill files under `skills/`

The root Node package graph will pin one exact `@xbluesky/cc-marketspec` version under `devDependencies`. Generated output lives at `.cc-marketspec/dist/manifest.json` and is ignored by Git.

Before each site build:

1. Install the committed root and site dependency graphs.
2. Run cc-marketspec validation without writing output.
3. Generate `.cc-marketspec/dist/manifest.json`.
4. Build the Astro site from that manifest.
5. Copy the same generated manifest into `site/dist/manifest.json`.

Astro reads the manifest at build time. It does not fetch the manifest from the deployed site at runtime. A manifest schema or consumer mismatch therefore fails the build instead of producing an incomplete live page.

The cc-marketspec entry will include an authored tagline, intro, and trigger coverage for every native cc-loadout skill. Groups referenced by the entry will be declared in the catalog. Tips, traps, or component-level copy will be added only where supported by the current cc-marketspec authoring guide.

## Repository Structure

The intended new surface is:

```text
.cc-marketspec/
  catalog.yaml
  entries/
    plugin-cc-loadout.yaml
  dist/                    # generated and ignored
.github/workflows/
  manifest.yml             # read-only manifest validation/build artifact flow
  site.yml                 # complete site build validation
site/
  public/
  scripts/
  src/
    components/
    layouts/
    pages/
    scripts/
    styles/
  package.json
  package-lock.json
  astro.config.mjs
  tsconfig.json
package.json               # exact cc-marketspec devDependency and root scripts
package-lock.json
```

Component boundaries will follow page responsibilities rather than animation implementation details. Each scene component owns its semantic markup; a small motion module coordinates scene timelines without becoming the source of content.

## Validation and Failure Behavior

- cc-marketspec schema errors fail local and CI builds.
- Missing required site data fails with a named field and source file rather than silently substituting fake content.
- Coverage warnings remain visible but do not block unless cc-marketspec classifies them as errors.
- Astro build errors block both GitHub checks and Cloudflare Pages deployment.
- The site and public manifest always come from the same build output.
- There is no runtime data failure mode because the page is prerendered.

## Testing

The implementation will provide:

- cc-marketspec schema and coverage validation.
- A manifest consumer test that loads the real generated manifest and verifies the fields required by the page.
- A static build test that verifies `site/dist/index.html` and `site/dist/manifest.json`.
- Assertions that the generated manifest is not committed.
- Reduced-motion and no-JavaScript CSS/markup behavior designed into the component structure.
- Responsive styles for mobile, tablet, and desktop without requiring the desktop choreography on small screens.

## Continuous Integration and Deployment

GitHub Actions will run on pull requests and pushes to the default branch. The workflow will:

1. Check out the repository.
2. Install the committed Node dependency graphs with `npm ci`.
3. Validate cc-marketspec authored data read-only.
4. Generate the ignored manifest.
5. Build the site.
6. Verify the expected static outputs.

The workflow will not grant repository-write permission, commit generated files, or deploy with a Cloudflare API token.

Cloudflare Pages Git integration will watch the default branch and run the same locked build. Its output directory is `site/dist`. A short README section will document the one-time dashboard configuration. Cloudflare credentials remain in Cloudflare, not in the repository or GitHub Actions.

## Accessibility and SEO

- The static document uses semantic headings, sections, navigation, lists, links, buttons, and code blocks.
- Copy controls have visible labels and keyboard focus states.
- Contrast meets WCAG AA for normal text.
- Decorative animation layers are hidden from assistive technology.
- The page includes a skip link and preserves logical document order.
- Metadata includes a product-specific title, description, canonical URL support, Open Graph fields, X card fields, favicon, and social preview image.
- The homepage and manifest endpoint remain functional without client-side routing.

## Risks and Mitigations

### Motion overwhelms the product

Mitigation: one message per scene, restrained palette, real UI evidence, and motion tied to product relationships.

### Mobile performance degrades

Mitigation: simplified mobile timelines, shorter sticky distances, fewer composited layers, optimized imagery, and reduced-motion fallback.

### Plugin-first messaging hides the binary prerequisite

Mitigation: keep the plugin visually primary but present the binary as explicit installation step one and execution engine.

### Generated data drifts from authored sources

Mitigation: generate in every build, consume that exact output, and never commit the derived manifest.

### The site implies unsupported features

Mitigation: derive claims from native plugin metadata, bundled skills, README content, and real TUI assets. Do not fabricate UI or metrics.

## Acceptance Criteria

- The homepage is English-first and uses the Loadout Console direction.
- The Claude Code plugin is the recommended experience; CLI and TUI roles are accurate.
- The page contains all five approved scroll-story scenes.
- Desktop provides cinematic scroll-driven transitions without scroll hijacking.
- Mobile and reduced-motion users receive a complete, readable experience.
- The site builds statically under `site/`.
- cc-marketspec authored data is source-controlled and generated output is ignored.
- The site consumes the generated manifest at build time and publishes the same artifact at `/manifest.json`.
- Invalid marketplace data or an invalid site build blocks CI.
- GitHub Actions performs read-only validation and build checks.
- Cloudflare Pages Git integration can publish `site/dist` without repository-write credentials.
- The supplied logo remains intact and is used as the visual source.
