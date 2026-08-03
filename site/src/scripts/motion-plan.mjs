export function getMotionMode({ reduced, width }) {
  if (reduced) return 'reduced';
  return width < 768 ? 'mobile' : 'desktop';
}

export function setMotionMode(root, mode) {
  root.dataset.motion = mode;
}

export function watchMotionPreference(preference, onChange) {
  preference.addEventListener('change', onChange);
  return () => preference.removeEventListener('change', onChange);
}
