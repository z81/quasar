const buttons = () => [...document.querySelectorAll('[data-install-quasar]')];
let installPrompt;
function update() {
  const installed = matchMedia('(display-mode: standalone)').matches || navigator.standalone === true;
  for (const button of buttons()) {
    button.disabled = !installPrompt || installed;
    const text = installed ? 'Приложение установлено' : 'Установить Quasar';
    if (button.textContent !== text) button.textContent = text;
  }
  for (const note of document.querySelectorAll('[data-install-quasar-note]')) {
    const text = installed ? 'Пульт открыт как отдельное приложение.'
      : !isSecureContext ? 'Для установки приложения откройте пульт через HTTPS. В браузере пульт работает и по текущему адресу.'
      : installPrompt ? 'Пульт откроется в отдельном окне. Для управления колонкой сервер должен оставаться доступен.'
      : 'Если кнопка недоступна, используйте меню браузера «Установить приложение» или «На экран Домой».';
    if (note.textContent !== text) note.textContent = text;
  }
}
addEventListener('beforeinstallprompt', event => { event.preventDefault(); installPrompt = event; update(); });
addEventListener('appinstalled', () => { installPrompt = undefined; update(); });
document.addEventListener('click', async event => {
  if (!event.target.closest('[data-install-quasar]') || !installPrompt) return;
  const prompt = installPrompt; installPrompt = undefined; update();
  await prompt.prompt(); await prompt.userChoice;
});
new MutationObserver(update).observe(document.body, {childList: true, subtree: true});
if ('serviceWorker' in navigator && isSecureContext) {
  navigator.serviceWorker.register('/ui/sw.js', {scope: '/ui/'}).catch(() => {});
}
update();
