# Task 4 Report: Progressive Cinematic Scroll Motion

## Implementation

- Added a pure motion policy that gives reduced motion precedence, uses simplified choreography below 768px, and keeps the document mode synchronized when GSAP media contexts change.
- Added progressive GSAP ScrollTrigger enhancement without changing Task 3's semantic markup or static content states.
- Built five deliberate loadout-case beats: hero case approach and seating, loose-tool filtering into the active rail, profile trays stacking into place, the real TUI mounting into its chassis, and installation channels extending from the service mat.
- Kept native scrolling. Desktop uses three scrubbed pinned scenes plus two scrubbed mounting sequences; mobile uses short one-time transform/opacity reveals without pinning.
- Added idempotent initialization: every `initMotion()` call reverts the previous match-media context and removes its pagehide listener before creating a new one. Media-query teardown reverts ScrollTriggers and GSAP inline states.
- Reinitializes when the live reduced-motion preference changes and when a persisted page returns from the back/forward cache.
- Added mode-specific desktop/mobile spacing and reduced-motion rules that force completed, readable transform/opacity states and disable smooth scrolling.
- Imported the motion entry once from the homepage composition root.

## TDD Evidence

### Motion policy RED/GREEN

`node --test site/test/motion-plan.test.mjs` first failed with `ERR_MODULE_NOT_FOUND` for `site/src/scripts/motion-plan.mjs`. After the minimal policy implementation, all three required mode-selection tests passed.

### Resize mode synchronization RED/GREEN

A focused document-mode synchronization contract then failed because `setMotionMode` was not exported. After adding the small setter and using it at initialization and inside both GSAP media contexts, the four motion-policy tests passed.

### Review findings RED/GREEN

The pre-commit review identified that Astro emitted the motion module after `</html>` and that the reduced-motion preference was sampled only at startup. A homepage contract failed against the invalid script position, and a preference-observer contract failed because the watcher did not exist. Moving the script inside the Layout content and adding cleaned preference-change reinitialization made both contracts pass. Persisted `pageshow` also reinitializes motion after pagehide cleanup.

## Verification

Required command:

```sh
npm run site:build && node --test site/test/motion-plan.test.mjs site/test/homepage.test.mjs
```

Result: Astro built one static page and the client bundle without errors; all 10 focused assertions passed.

Full covering command:

```sh
npm run site:verify
```

Result: static build passed, the manifest contract passed 1/1, and all site tests passed 17/17.

Additional checks:

- `git diff --check` passed.
- Built-output audit found all five scenes in static HTML, zero authored inline styles, one external motion bundle, three desktop pins, and match-media/pagehide/revert lifecycle code in the emitted client bundle.
- The motion module never calls a scroll normalizer or ScrollSmoother; scrolling remains native.
- Base CSS keeps horizontal overflow clipped on the body, while all wide animation travel is transform-only and mode-gated.
- Static HTML contains the complete product story before the client bundle runs; CSS does not hide motion targets by default.
- The pre-existing untracked root `logo.png` was not modified or staged.

## Browser Verification Limitation

The local Astro preview server started successfully, but the session reported no available browser backend. Desktop/mobile screenshots, scroll scrubbing, computed overflow measurements, and live console inspection therefore remain for the controller's visual verification pass. This is non-blocking for the source/build task and is not represented as completed browser evidence.

## Review

Pre-commit independent review found no Critical issues and two Important lifecycle/document-wiring issues. Both were fixed with focused red/green contracts: the client script now renders before `</body>`, and preference changes cleanly reinitialize motion. The review's BFCache observation was also addressed with persisted `pageshow` reinitialization.

## Concerns

- Live browser verification remains outstanding as documented above.
