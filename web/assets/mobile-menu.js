// A native dialog supplies focus trapping, Escape handling and focus restoration.
const mobile = matchMedia('(max-width: 760px)');
const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');
const interactive = 'input, textarea, select, button, a, label, summary, [contenteditable]:not([contenteditable="false"]), [role="slider"], [role="button"], canvas, .device-terminal-shell, .midi-preview';
let gesture;
let closingTimer;
const menu = () => document.getElementById('mobile-menu');
const resetGesture = () => { gesture = undefined; };

function closeMenu() {
  const dialog = menu();
  if (!dialog?.open || dialog.classList.contains('closing')) return;
  if (reducedMotion.matches) { dialog.close(); return; }
  dialog.classList.add('closing');
  closingTimer = setTimeout(() => dialog.close(), 200);
}

document.addEventListener('click', event => {
  const dialog = menu();
  if (!dialog || !(event.target instanceof Element)) return;
  if (event.target.closest('.mobile-menu-toggle')) {
    if (mobile.matches && !dialog.open) dialog.showModal();
  } else if (dialog.open && (event.target === dialog || event.target.closest('.mobile-menu-close, .mobile-menu-navigation .nav-item'))) {
    closeMenu();
  }
});
document.addEventListener('cancel', event => {
  if (event.target !== menu()) return;
  event.preventDefault();
  closeMenu();
}, true);
document.addEventListener('close', event => {
  if (event.target.id !== 'mobile-menu') return;
  clearTimeout(closingTimer);
  event.target.classList.remove('closing');
  resetGesture();
}, true);
mobile.addEventListener('change', () => {
  resetGesture();
  if (!mobile.matches) menu()?.close();
});

document.addEventListener('touchstart', event => {
  resetGesture();
  const target = event.target;
  const dialog = menu();
  if (!mobile.matches || !dialog || event.touches.length !== 1 || !(target instanceof Element)) return;
  if (window.visualViewport?.scale > 1.05) return;
  if (window.getSelection()?.toString()) return;
  const closing = dialog.open;
  if (!target.closest(closing ? '.mobile-menu' : '.workspace')) return;
  // A closing swipe may start on a menu item; other controls keep their gestures.
  if (!(closing && target.closest('.mobile-menu-navigation .nav-item')) && target.closest(interactive)) return;
  if (!closing && document.activeElement?.matches('input, textarea, select, [contenteditable="true"]')) return;
  for (let node = target; node && node !== document.body; node = node.parentElement) {
    if (node.scrollWidth > node.clientWidth + 1 && /auto|scroll/.test(getComputedStyle(node).overflowX)) return;
  }
  const touch = event.touches[0];
  gesture = { id: touch.identifier, x: touch.clientX, y: touch.clientY, time: event.timeStamp, scrollY, closing, dialog };
}, { passive: true });
document.addEventListener('touchmove', event => {
  if (!gesture) return;
  const touch = [...event.touches].find(t => t.identifier === gesture.id);
  if (event.touches.length !== 1 || !touch) { resetGesture(); return; }
  const dx = Math.abs(touch.clientX - gesture.x);
  const dy = Math.abs(touch.clientY - gesture.y);
  if ((dy > 12 && dy >= dx) || Math.abs(scrollY - gesture.scrollY) > 16) resetGesture();
}, { passive: true });
document.addEventListener('touchend', event => {
  const start = gesture;
  resetGesture();
  if (!start || event.touches.length || !mobile.matches || start.dialog !== menu()) return;
  const touch = [...event.changedTouches].find(t => t.identifier === start.id);
  if (!touch || event.timeStamp - start.time > 800 || window.getSelection()?.toString()) return;
  const dx = touch.clientX - start.x;
  const dy = Math.abs(touch.clientY - start.y);
  if (Math.abs(dx) < 80 || Math.abs(dx) < dy * 1.8 || Math.abs(scrollY - start.scrollY) > 16) return;
  if (start.closing && dx < 0 && start.dialog.open) {
    event.preventDefault();
    closeMenu();
  } else if (!start.closing && dx > 0 && !start.dialog.open) {
    event.preventDefault();
    start.dialog.showModal();
  }
}, { passive: false });
document.addEventListener('touchcancel', resetGesture, { passive: true });
window.addEventListener('pagehide', resetGesture);
