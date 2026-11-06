const instances = new Map();
const engines = new Map();
const motion = matchMedia('(prefers-reduced-motion: reduce)');
const clamp = (value, min, max) => Math.min(max, Math.max(min, value));
const brightness = value => /^\+?\d+$/.test(value) && Number(value) <= 100 ? Number(value) / 100 : 0;

function roundedShape(T, size, radius) {
  const s = new T.Shape(), h = size / 2, r = radius;
  s.moveTo(-h + r, -h); s.lineTo(h - r, -h); s.quadraticCurveTo(h, -h, h, -h + r);
  s.lineTo(h, h - r); s.quadraticCurveTo(h, h, h - r, h); s.lineTo(-h + r, h);
  s.quadraticCurveTo(-h, h, -h, h - r); s.lineTo(-h, -h + r); s.quadraticCurveTo(-h, -h, -h + r, -h);
  return s;
}

function createModel(root, T) {
  const canvas = root.querySelector('canvas');
  const renderer = new T.WebGLRenderer({ canvas, alpha: true, antialias: true, powerPreference: 'low-power' });
  renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
  renderer.outputColorSpace = T.SRGBColorSpace;
  renderer.toneMapping = T.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.2;
  const scene = new T.Scene(), model = new T.Group();
  scene.add(model);
  const camera = new T.PerspectiveCamera(35, 1, .1, 20);
  camera.position.set(0, 1.05, 3.15); camera.lookAt(0, .04, 0);
  scene.add(new T.HemisphereLight(0xe7edff, 0x4b425e, 2.4));
  const key = new T.DirectionalLight(0xfff5eb, 4); key.position.set(3, 4, 4); scene.add(key);
  const fill = new T.DirectionalLight(0xb4c9ff, 2); fill.position.set(-3, 1, 2); scene.add(fill);
  const rim = new T.DirectionalLight(0xc0a2ff, 3); rim.position.set(-2, 3, -3); scene.add(rim);
  const glow = new T.PointLight(0xa855f7, .3, 2); glow.position.set(0, .85, .25); model.add(glow);
  const textures = new Set();
  const texture = (width, height, draw) => {
    const surface = document.createElement('canvas'); surface.width = width; surface.height = height;
    const context = surface.getContext('2d'); draw?.(context, width, height);
    const map = new T.CanvasTexture(surface); map.colorSpace = T.SRGBColorSpace; textures.add(map);
    return { map, context, surface };
  };
  const fabric = texture(64, 64, context => {
    context.fillStyle = '#a2a2a6'; context.fillRect(0, 0, 64, 64);
    for (let y = 0; y < 64; y += 4) for (let x = 0; x < 64; x += 4) {
      context.strokeStyle = '#66686d'; context.lineWidth = 1.2; context.beginPath();
      context.moveTo(x, y); context.lineTo(x + 2, y + 3); context.stroke();
      context.strokeStyle = '#d0cfd3'; context.beginPath(); context.moveTo(x + 2, y); context.lineTo(x + 3, y + 2); context.stroke();
    }
  }).map;
  fabric.wrapS = fabric.wrapT = T.RepeatWrapping; fabric.repeat.set(5, 8); fabric.anisotropy = Math.min(4, renderer.capabilities.getMaxAnisotropy());
  const cloth = new T.MeshStandardMaterial({ color: 0x373840, map: fabric, bumpMap: fabric, bumpScale: .006, roughness: .95 });
  const plastic = new T.MeshStandardMaterial({ color: 0x202127, roughness: .36, metalness: .2 });
  const black = new T.MeshStandardMaterial({ color: 0x101116, roughness: .8 });
  function body(size, height, y, material, bevel = .009) {
    const geometry = new T.ExtrudeGeometry(roundedShape(T, size, .1), {
      depth: height, steps: 1, bevelEnabled: true, bevelThickness: bevel, bevelSize: bevel, bevelSegments: 2, curveSegments: 8,
    });
    geometry.rotateX(-Math.PI / 2); geometry.translate(0, y - height / 2, 0);
    const mesh = new T.Mesh(geometry, material); model.add(mesh); return mesh;
  }
  body(.96, 1.08, 0, cloth);
  body(.947, .026, .566, black, .003);
  body(.958, .025, .599, plastic, .006);
  body(.91, .025, -.555, black, .004);

  const ringPoints = roundedShape(T, .96, .1).getSpacedPoints(96);
  const ring = [];
  for (let i = 0; i < 24; i++) {
    const points = ringPoints.slice(i * 4, i * 4 + 5).map(p => new T.Vector3(p.x, .572, -p.y));
    const path = new T.CatmullRomCurve3(points);
    const material = new T.MeshBasicMaterial({ color: 0xa855f7, toneMapped: false });
    const mesh = new T.Mesh(new T.TubeGeometry(path, 6, .0045, 4, false), material); model.add(mesh);
    const haloMaterial = new T.MeshBasicMaterial({ color: 0xa855f7, transparent: true, opacity: .15, depthWrite: false, blending: T.AdditiveBlending, toneMapped: false });
    model.add(new T.Mesh(new T.TubeGeometry(path, 6, .012, 5, false), haloMaterial));
    ring.push({ material, haloMaterial });
  }
  const logoBase = new T.Mesh(new T.CylinderGeometry(.044, .044, .003, 24), black);
  logoBase.position.set(0, .621, .025); model.add(logoBase);
  const logo = [];
  for (let i = 0; i < 6; i++) {
    const material = new T.MeshBasicMaterial({ color: 0xa855f7, toneMapped: false });
    const dot = new T.Mesh(new T.SphereGeometry(.009, 10, 6), material);
    const angle = i * Math.PI / 3;
    dot.scale.y = .22; dot.position.set(Math.cos(angle) * .021, .624, .025 + Math.sin(angle) * .021); model.add(dot);
    logo.push({ material });
  }
  const buttonInk = new T.MeshStandardMaterial({ color: 0x96939f, roughness: .7 });
  const line = (x, z, vertical) => {
    const mesh = new T.Mesh(new T.BoxGeometry(vertical ? .008 : .047, .002, vertical ? .047 : .008), buttonInk);
    mesh.position.set(x, .626, z); model.add(mesh);
  };
  line(-.28, .025, false); line(.28, .025, false); line(.28, .025, true);
  const mute = new T.Mesh(new T.CylinderGeometry(.012, .012, .002, 16), buttonInk); mute.position.set(0, .626, -.29); model.add(mute);
  const jack = new T.Mesh(new T.CircleGeometry(.025, 20), black); jack.position.set(0, -.42, -.492); jack.rotation.y = Math.PI; model.add(jack);

  const display = texture(300, 92);
  const screen = new T.Mesh(new T.PlaneGeometry(.7, .215), new T.MeshBasicMaterial({ map: display.map, transparent: true, depthWrite: false, toneMapped: false }));
  screen.position.set(0, .12, .492); model.add(screen);
  const layout = JSON.parse(root.dataset.layout).map(segment => ({ ...segment, points: segment.points.split(' ').map(pair => pair.split(',').map(Number)) }));
  function updateDisplay() {
    const frame = JSON.parse(root.dataset.display), lit = new Set(frame.segments), ctx = display.context;
    ctx.clearRect(0, 0, 300, 92);
    if (frame.intensity > 0) for (const segment of layout) {
      if (!lit.has(segment.id)) continue;
      ctx.save(); ctx.translate(segment.x, segment.y); ctx.beginPath();
      segment.points.forEach(([x, y], index) => index ? ctx.lineTo(x, y) : ctx.moveTo(x, y)); ctx.closePath();
      ctx.fillStyle = `rgba(190,215,255,${frame.intensity * .2})`; ctx.shadowColor = '#b1d1ff'; ctx.shadowBlur = 3; ctx.fill(); ctx.shadowBlur = 0; ctx.clip();
      ctx.fillStyle = `rgba(225,239,255,${frame.intensity})`;
      const xs = segment.points.map(p => p[0]), ys = segment.points.map(p => p[1]);
      for (let y = Math.floor(Math.min(...ys) / 3) * 3; y <= Math.max(...ys); y += 3) {
        for (let x = Math.floor(Math.min(...xs) / 3) * 3; x <= Math.max(...xs); x += 3) { ctx.beginPath(); ctx.arc(x + 1, y + 1, 1.1, 0, Math.PI * 2); ctx.fill(); }
      }
      ctx.restore();
    }
    display.map.needsUpdate = true;
  }
  const shadow = texture(128, 128, ctx => {
    const gradient = ctx.createRadialGradient(64, 64, 8, 64, 64, 64); gradient.addColorStop(0, '#00000075'); gradient.addColorStop(1, '#00000000');
    ctx.fillStyle = gradient; ctx.fillRect(0, 0, 128, 128);
  }).map;
  const floor = new T.Mesh(new T.PlaneGeometry(1.9, 1.7), new T.MeshBasicMaterial({ map: shadow, transparent: true, depthWrite: false, toneMapped: false }));
  floor.rotation.x = -Math.PI / 2; floor.position.y = -.585; scene.add(floor);

  let preview = JSON.parse(root.dataset.preview), visible = true, disposed = false, contextLost = false, raf = 0, lastFrame = 0, drag;
  const initialYaw = -.48, initialPitch = .04;
  let yaw = initialYaw, pitch = initialPitch;
  model.rotation.set(pitch, yaw, 0);
  const tint = new T.Color();
  function lights(leds, draft, time) {
    const level = brightness(draft.brightness), color = /^#[\da-f]{6}$/i.test(draft.color) ? draft.color : '#a855f7';
    for (let i = 0; i < leds.length; i++) {
      let power = 1;
      if (!motion.matches) {
        if (draft.animation === 'pulse') power = .15 + .85 * (1 - Math.cos(time * Math.PI)) / 2;
        else if (draft.animation === 'wave') { const phase = (time / (leds.length * .1) + i / leds.length) % 1; power = phase < .25 ? 1 - phase * 4 : phase > .75 ? (phase - .75) * 4 : 0; }
      }
      if (draft.animation === 'rainbow') tint.setHSL((i / leds.length + (motion.matches ? 0 : time / 4)) % 1, .85, .55, T.SRGBColorSpace);
      else tint.set(color);
      leds[i].material.color.copy(tint).multiplyScalar(level * power * 1.8);
      if (leds[i].haloMaterial) { leds[i].haloMaterial.color.copy(tint); leds[i].haloMaterial.opacity = level * power * .22; }
    }
    return level;
  }
  function schedule() { if (!disposed && !contextLost && visible && !document.hidden && !raf) raf = requestAnimationFrame(render); }
  function render(now) {
    raf = 0;
    if (disposed || contextLost || !visible || document.hidden) return;
    if (now - lastFrame < 1000 / 30) { schedule(); return; }
    lastFrame = now;
    const easing = motion.matches ? 1 : .22;
    model.rotation.y += (yaw - model.rotation.y) * easing;
    model.rotation.x += (pitch - model.rotation.x) * easing;
    const time = now / 1000;
    glow.intensity = lights(ring, preview.ring, time) * .5;
    glow.color.set(/^#[\da-f]{6}$/i.test(preview.ring.color) ? preview.ring.color : '#a855f7');
    lights(logo, preview.logo, time);
    renderer.render(scene, camera);
    root.dataset.modelState = 'ready';
    if (!motion.matches || Math.abs(yaw - model.rotation.y) + Math.abs(pitch - model.rotation.x) > .001) schedule();
  }
  const resize = new ResizeObserver(() => {
    const { width, height } = canvas.getBoundingClientRect();
    if (!width || !height) return;
    renderer.setSize(width, height, false); camera.aspect = width / height; camera.updateProjectionMatrix(); schedule();
  });
  resize.observe(canvas);
  const attributes = new MutationObserver(() => { preview = JSON.parse(root.dataset.preview); updateDisplay(); schedule(); });
  attributes.observe(root, { attributes: true, attributeFilter: ['data-preview', 'data-display'] });
  const down = event => {
    if (!event.isPrimary || event.button !== 0) return;
    drag = { id: event.pointerId, x: event.clientX, y: event.clientY, yaw, pitch };
    canvas.setPointerCapture(event.pointerId);
  };
  const move = event => {
    if (!drag || drag.id !== event.pointerId) return;
    const dx = event.clientX - drag.x, dy = event.clientY - drag.y;
    if (event.pointerType === 'touch' && Math.abs(dy) > Math.abs(dx)) return;
    yaw = drag.yaw + dx * .012;
    if (event.pointerType !== 'touch') pitch = clamp(drag.pitch + dy * .006, -.35, .55);
    schedule();
  };
  const up = () => { drag = undefined; };
  const keydown = event => {
    if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home'].includes(event.key)) return;
    event.preventDefault();
    if (event.key === 'Home') { yaw = initialYaw; pitch = initialPitch; }
    else if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') yaw += event.key === 'ArrowLeft' ? -.2 : .2;
    else pitch = clamp(pitch + (event.key === 'ArrowUp' ? -.1 : .1), -.35, .55);
    schedule();
  };
  const click = event => {
    const button = event.target.closest('button');
    if (button?.hasAttribute('data-reset-model')) { yaw = initialYaw; pitch = initialPitch; schedule(); }
    else if (button?.dataset.turn) { yaw += Number(button.dataset.turn) * Math.PI / 4; schedule(); }
  };
  const lost = event => { event.preventDefault(); contextLost = true; cancelAnimationFrame(raf); raf = 0; root.dataset.modelState = 'unavailable'; };
  const restored = () => { contextLost = false; schedule(); };
  const visibility = () => { if (document.hidden) { cancelAnimationFrame(raf); raf = 0; } else schedule(); };
  const listeners = [[canvas, 'pointerdown', down], [canvas, 'pointermove', move], [canvas, 'pointerup', up], [canvas, 'pointercancel', up], [canvas, 'lostpointercapture', up], [canvas, 'keydown', keydown], [root, 'click', click], [canvas, 'webglcontextlost', lost], [canvas, 'webglcontextrestored', restored], [document, 'visibilitychange', visibility], [motion, 'change', schedule]];
  listeners.forEach(([target, name, handler]) => target.addEventListener(name, handler));
  updateDisplay(); schedule();
  return {
    setVisible(value) { visible = value; if (visible) schedule(); else { cancelAnimationFrame(raf); raf = 0; } },
    dispose() {
      disposed = true; cancelAnimationFrame(raf); resize.disconnect(); attributes.disconnect();
      listeners.forEach(([target, name, handler]) => target.removeEventListener(name, handler));
      const geometries = new Set(), materials = new Set();
      scene.traverse(object => { if (object.geometry) geometries.add(object.geometry); if (object.material) (Array.isArray(object.material) ? object.material : [object.material]).forEach(m => materials.add(m)); });
      geometries.forEach(g => g.dispose()); materials.forEach(m => m.dispose()); textures.forEach(t => t.dispose());
      renderer.dispose(); renderer.forceContextLoss();
    },
  };
}

const visibility = new IntersectionObserver(entries => {
  for (const entry of entries) {
    const root = entry.target, record = instances.get(root);
    if (!record) continue;
    record.visible = entry.isIntersecting;
    record.model?.setVisible(record.visible);
    if (!record.visible || record.loading || record.model || record.failed) continue;
    record.loading = true;
    const url = root.dataset.engine;
    if (!engines.has(url)) engines.set(url, import(url));
    engines.get(url).then(T => {
      if (!root.isConnected || instances.get(root) !== record) return;
      record.model = createModel(root, T); record.model.setVisible(record.visible);
    }).catch(() => { record.failed = true; if (root.isConnected) root.dataset.modelState = 'unavailable'; });
  }
}, { rootMargin: '120px' });
function scan() {
  for (const root of document.querySelectorAll('.station-model')) if (!instances.has(root)) { instances.set(root, { visible: false }); visibility.observe(root); }
  for (const [root, record] of instances) if (!root.isConnected) { visibility.unobserve(root); record.model?.dispose(); instances.delete(root); }
}
new MutationObserver(scan).observe(document.body, { childList: true, subtree: true });
scan();
