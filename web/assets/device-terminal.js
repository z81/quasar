// xterm.js 6.0.0 + addon-fit 0.11.0 are served locally. See vendor/*LICENSE.
let libraries;
function script(url) {
  return new Promise((resolve, reject) => {
    const node = document.createElement('script');
    node.src = url;
    node.onload = resolve;
    node.onerror = () => reject(new Error('Не удалось загрузить терминал. Обновите страницу.'));
    document.head.appendChild(node);
  });
}

export async function createTerminal(config, report) {
  libraries ??= script(config.xterm).then(() => script(config.fit)).catch(error => {
    libraries = undefined;
    throw error;
  });
  await libraries;
  const element = document.getElementById(config.element);
  if (!element) throw new Error('Окно терминала закрыто.');
  let active = true;
  let socket;
  let connected = false;
  let pendingUpload;
  let pendingWrite;
  const emit = value => { if (active) report(value); };
  const terminal = new window.Terminal({
    cursorBlink: true, fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
    fontSize: 13, scrollback: 2000, allowProposedApi: false,
    screenReaderMode: true,
  });
  const fit = new window.FitAddon.FitAddon();
  terminal.loadAddon(fit);
  terminal.open(element);
  const send = message => {
    if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(message));
  };
  const resize = () => {
    if (!active || element.clientWidth === 0 || element.clientHeight === 0) return;
    fit.fit();
    if (connected) send({ type: 'resize', cols: terminal.cols, rows: terminal.rows });
  };
  const observer = new ResizeObserver(resize);
  observer.observe(element);
  terminal.onData(data => {
    if (!connected) return;
    // Keep each input message within the server's 16 KiB UTF-8 limit.
    const characters = Array.from(data);
    for (let i = 0; i < characters.length; i += 4096) send({ type: 'input', data: characters.slice(i, i + 4096).join('') });
  });
  const theme = name => {
    const dark = name === 'dark';
    terminal.options.theme = dark
      ? { background: '#171916', foreground: '#e7e9e2', cursor: '#bcd4a5', selectionBackground: '#53634a', black: '#171916', brightBlack: '#94998d' }
      : { background: '#f6f7f3', foreground: '#292d25', cursor: '#435c33', selectionBackground: '#d6e1c9', black: '#292d25', brightBlack: '#626a59', white: '#65705c', brightWhite: '#292d25', yellow: '#8b6200', brightYellow: '#8b6200', green: '#377228', brightGreen: '#377228', cyan: '#14767a', brightCyan: '#14767a' };
  };
  theme(config.theme);
  resize();
  const disconnect = () => {
    connected = false;
    if (socket) { socket.onopen = socket.onmessage = socket.onerror = socket.onclose = null; socket.close(); socket = undefined; }
  };
  const dispose = () => {
    if (!active) return;
    active = false; disconnect(); observer.disconnect(); lifecycle.disconnect();
    window.removeEventListener('pagehide', dispose);
    terminal.dispose(); pendingUpload = pendingWrite = undefined;
  };
  const lifecycle = new MutationObserver(() => { if (!element.isConnected) dispose(); });
  lifecycle.observe(document.body, { childList: true, subtree: true });
  window.addEventListener('pagehide', dispose);
  return {
    command(message) {
      if (!active) return;
      if (message.type === 'theme') { theme(message.theme); return; }
      if (message.type === 'connect') {
        disconnect(); terminal.reset();
        const url = new URL(`/devices/${encodeURIComponent(config.duid)}/${config.files ? 'files' : 'terminal'}`, location.href);
        url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
        url.searchParams.set('host', message.host); url.searchParams.set('port', message.port);
        socket = new WebSocket(url);
        socket.onmessage = event => {
          try {
            const value = JSON.parse(event.data);
            if (value.type === 'output') {
              terminal.write(Uint8Array.from(atob(value.data), char => char.charCodeAt(0)));
            } else if (value.type === 'connected') {
              connected = true; resize(); terminal.focus(); emit(value);
            } else if (value.type === 'closed') {
              connected = false; emit(value);
            } else if (value.type === 'shell_ready') {
              connected = false; emit(value);
            } else if (value.type === 'file') {
              const bytes = Uint8Array.from(atob(value.data), char => char.charCodeAt(0));
              let text = null;
              try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes); if (text.includes('\0')) text = null; } catch (_) { /* Binary files remain downloadable. */ }
              emit({ ...value, text, size: bytes.length });
            } else if (value.type === 'written' && pendingWrite?.path === value.path) {
              emit({ ...value, data: pendingWrite.data, size: pendingWrite.size }); pendingWrite = undefined;
            } else { emit(value); }
          } catch (_) { emit({ type: 'error', message: 'Не удалось прочитать ответ терминала.' }); }
        };
        socket.onerror = () => emit({ type: 'error', message: 'Соединение не установлено. Проверьте адрес, порт и доступность колонки из сети сервера.' });
        socket.onclose = () => { connected = false; emit({ type: 'closed' }); };
        return;
      }
      if (message.type === 'disconnect') { disconnect(); emit({ type: 'closed' }); return; }
      if (message.type === 'clear') { terminal.clear(); terminal.focus(); return; }
      if (message.type === 'download') {
        const bytes = Uint8Array.from(atob(message.data), char => char.charCodeAt(0));
        const url = URL.createObjectURL(new Blob([bytes], { type: 'application/octet-stream' }));
        const link = document.createElement('a'); link.href = url; link.download = message.name; link.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000); return;
      }
      if (message.type === 'choose_upload') {
        const input = document.createElement('input'); input.type = 'file';
        input.onchange = async () => {
          const file = input.files?.[0]; if (!file) return;
          if (file.size > 262144) { emit({ type: 'error', message: 'Файл должен быть не больше 256 КиБ.' }); return; }
          const bytes = new Uint8Array(await file.arrayBuffer());
          let binary = ''; for (let i = 0; i < bytes.length; i += 8192) binary += String.fromCharCode(...bytes.subarray(i, i + 8192));
          pendingUpload = { name: file.name, data: btoa(binary) };
          emit({ type: 'upload_selected', name: file.name, size: bytes.length });
        };
        input.click(); return;
      }
      if (message.type === 'upload') {
        if (pendingUpload) { send({ type: 'write', path: message.path, data: pendingUpload.data }); pendingUpload = undefined; }
        return;
      }
      if (message.type === 'save_text') {
        const bytes = new TextEncoder().encode(message.text);
        let binary = ''; for (let i = 0; i < bytes.length; i += 8192) binary += String.fromCharCode(...bytes.subarray(i, i + 8192));
        pendingWrite = { path: message.path, data: btoa(binary), size: bytes.length };
        send({ type: 'write', path: message.path, data: pendingWrite.data }); return;
      }
      send(message);
    },
    dispose,
  };
}
