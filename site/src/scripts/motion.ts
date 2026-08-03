import gsap from 'gsap';
import ScrollTrigger from 'gsap/ScrollTrigger';
import { getMotionMode, setMotionMode, watchMotionPreference } from './motion-plan.mjs';

gsap.registerPlugin(ScrollTrigger);

let revertActiveMotion: (() => void) | undefined;

export function initMotion() {
  revertActiveMotion?.();
  revertActiveMotion = undefined;

  const preference = window.matchMedia('(prefers-reduced-motion: reduce)');
  const reduced = preference.matches;
  const mode = getMotionMode({ reduced, width: window.innerWidth });
  setMotionMode(document.documentElement, mode);

  let mm: ReturnType<typeof gsap.matchMedia> | undefined;
  let reverted = false;
  const reinitialize = () => initMotion();
  const unwatchPreference = watchMotionPreference(preference, reinitialize);
  const revert = () => {
    if (reverted) return;
    reverted = true;
    window.removeEventListener('pagehide', revert);
    unwatchPreference();
    mm?.revert();
  };

  revertActiveMotion = revert;
  window.addEventListener('pagehide', revert, { once: true });

  if (mode === 'reduced') return;

  mm = gsap.matchMedia();

  mm.add('(min-width: 768px)', () => {
    setMotionMode(document.documentElement, 'desktop');
    const context = gsap.context(() => {
      gsap
        .timeline({
          defaults: { ease: 'none' },
          scrollTrigger: {
            trigger: '#hero',
            start: 'top top',
            end: '+=150%',
            scrub: 0.65,
            pin: true,
            anticipatePin: 1,
          },
        })
        .fromTo(
          '#hero .brand-frame',
          { x: '15vw', scale: 0.74, rotate: -6, transformOrigin: '50% 55%' },
          { x: 0, scale: 1.06, rotate: 0.5 },
          0,
        )
        .to('#hero h1', { y: '-11vh' }, 0)
        .from('#hero .eyebrow', { x: '-5vw', opacity: 0.2 }, 0.04)
        .from('#hero .hero-copy, #hero .hero-detail', { y: 52, opacity: 0.01, stagger: 0.12 }, 0.18)
        .from('#hero .hero-actions', { y: 34, opacity: 0.01 }, 0.34)
        .to('#hero .brand-frame', { y: '-5vh', scale: 0.94, rotate: 2.5 }, 0.72);

      gsap
        .timeline({
          defaults: { ease: 'none' },
          scrollTrigger: {
            trigger: '#problem',
            start: 'top top',
            end: '+=135%',
            scrub: 0.65,
            pin: true,
            anticipatePin: 1,
          },
        })
        .from('#problem .problem-heading', { x: '-6vw', opacity: 0.2 }, 0)
        .from(
          '#problem .loose-tools .plugin-chip',
          { x: (index) => (index % 2 ? '10vw' : '-10vw'), y: (index) => `${(index % 3) * 8 - 8}vh`, scale: 0.72, opacity: 0.08, stagger: 0.035 },
          0,
        )
        .to(
          '#problem .loose-tools .plugin-chip:not(.chip-github):not(.chip-browser):not(.chip-files)',
          { x: (index) => (index % 2 ? '18vw' : '-18vw'), y: (index) => `${index % 2 ? -14 : 14}vh`, scale: 0.62, rotate: (index) => (index % 2 ? 18 : -18), opacity: 0.06, stagger: 0.025 },
          0.46,
        )
        .to(
          '#problem .chip-github, #problem .chip-browser, #problem .chip-files',
          { x: (index) => `${(index - 1) * 7}vw`, y: '25vh', scale: 0.78, rotate: 0, opacity: 0.08, stagger: 0.04 },
          0.46,
        )
        .from('#problem .active-loadout', { y: 92, scale: 0.84, opacity: 0.01, transformOrigin: '50% 100%' }, 0.48);

      gsap
        .timeline({
          defaults: { ease: 'none' },
          scrollTrigger: {
            trigger: '#profiles',
            start: 'top top',
            end: '+=150%',
            scrub: 0.65,
            pin: true,
            anticipatePin: 1,
          },
        })
        .from('#profiles .profiles-heading', { y: 48, opacity: 0.08 }, 0)
        .from(
          '#profiles .profile-band',
          { x: (index) => `${index % 2 ? 18 : -18}vw`, y: (index) => 80 - index * 20, rotate: (index) => (index - 1) * 1.5, scale: 0.94, opacity: 0.06, stagger: 0.12 },
          0.12,
        )
        .from(
          '#profiles [data-profile-card]',
          { y: 42, scale: 0.7, opacity: 0.01, stagger: 0.055, transformOrigin: '50% 100%' },
          0.42,
        )
        .to('#profiles .profile-band', { y: (index) => index * -10, stagger: 0.06 }, 0.78);

      gsap
        .timeline({
          defaults: { ease: 'none' },
          scrollTrigger: { trigger: '#visual-control', start: 'top 76%', end: 'bottom 46%', scrub: 0.45 },
        })
        .from('#visual-control .tui-heading', { x: '-6vw', opacity: 0.08 }, 0)
        .from('#visual-control .tui-frame', { y: 72, scale: 0.84, rotateX: 7, opacity: 0.01, transformOrigin: '50% 100%' }, 0.08)
        .from('#visual-control [data-tui-capability]', { y: 34, scale: 0.92, opacity: 0.01, stagger: 0.07 }, 0.44)
        .from('#visual-control .tui-brand-mark', { x: '8vw', rotate: 18, opacity: 0.01 }, 0.5);

      gsap
        .timeline({
          defaults: { ease: 'none' },
          scrollTrigger: { trigger: '#install', start: 'top 72%', end: '+=90%', scrub: 0.45 },
        })
        .from('#install .install-heading', { x: '-7vw', opacity: 0.08 }, 0)
        .from('#install .install-step', { x: (index) => `${index % 2 ? 9 : 5}vw`, y: 36, scale: 0.96, opacity: 0.01, stagger: 0.18 }, 0.1)
        .from('#install .command-block', { scaleX: 0.84, opacity: 0.08, stagger: 0.08, transformOrigin: '0% 50%' }, 0.34)
        .from('#install .install-cta', { y: 28, opacity: 0.01 }, 0.58);
    }, document.body);

    return () => context.revert();
  });

  mm.add('(max-width: 767px)', () => {
    setMotionMode(document.documentElement, 'mobile');
    const context = gsap.context(() => {
      gsap.utils.toArray<HTMLElement>('[data-motion-scene]').forEach((scene, sceneIndex) => {
        gsap.from(scene.children, {
          x: sceneIndex % 2 ? 14 : -14,
          y: 18,
          scale: 0.985,
          opacity: 0.18,
          duration: 0.58,
          stagger: 0.065,
          ease: 'power2.out',
          clearProps: 'transform,opacity',
          scrollTrigger: { trigger: scene, start: 'top 86%', once: true },
        });
      });
    }, document.body);

    return () => context.revert();
  });

}

window.addEventListener('pageshow', (event) => {
  if (event.persisted) initMotion();
});

initMotion();
