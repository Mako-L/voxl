// Toolbar (top right on hover; bottom right and always visible on touch devices):
// on-screen keyboard, extra keys, zoom, files, microphone and fullscreen buttons.
import type { App } from './app';
import { el } from './app';
import type { FitMode, Viewport } from './viewport';

const ZOOM_STEP = 1.25;

interface WebkitDocument {
	webkitFullscreenEnabled?: boolean;
	webkitFullscreenElement?: Element | null;
	webkitExitFullscreen?: () => void;
}
interface WebkitElement {
	webkitRequestFullscreen?: () => void;
}

const MODE_KEY = 'voxl.fitMode';
const MODES: FitMode[] = ['fill', 'fit', 'auto'];

/** Fill by default on a phone or tablet streaming a simulated phone, unless the
 * viewer chose otherwise before. */
export function defaultFill(viewport: Viewport, sim: boolean) {
	let saved: string | null = null;
	try {
		saved = localStorage.getItem(MODE_KEY);
	} catch {}
	const touch = matchMedia('(pointer: coarse)').matches;
	viewport.setMode(
		MODES.includes(saved as FitMode)
			? (saved as FitMode)
			: touch && sim
				? 'fill'
				: 'fit'
	);
}

export function setupToolbar(app: App, viewport: Viewport) {
	const { tools, kbd } = app;
	const kbBtn = el<HTMLButtonElement>('kb-btn');
	const fsBtn = el<HTMLButtonElement>('fs-btn');
	const zoomIn = el<HTMLButtonElement>('zoom-in');
	const zoomOut = el<HTMLButtonElement>('zoom-out');
	const zoomReset = el<HTMLButtonElement>('zoom-reset');
	const fillBtn = el<HTMLButtonElement>('fill-btn');
	const hint = el('hint');
	let hideTimer: ReturnType<typeof setTimeout> | undefined;
	function show(ms: number) {
		tools.classList.add('show');
		clearTimeout(hideTimer);
		if (ms) hideTimer = setTimeout(() => tools.classList.remove('show'), ms);
	}
	// Touch devices keep the toolbar visible; mice reveal it near the top edge.
	const touch = matchMedia('(pointer: coarse)').matches;
	if (touch) {
		tools.classList.add('touch');
		show(0);
	} else
		document.addEventListener('pointermove', (e) => {
			if (
				e.pointerType === 'mouse' &&
				e.clientY < 48 &&
				e.clientX > window.innerWidth - 260
			)
				show(2500);
		});
	// Clicks on the toolbar must not reach the desktop surface underneath, nor take
	// focus from the keyboard's textarea (closing a phone's keyboard).
	for (const b of tools.querySelectorAll('button')) {
		b.addEventListener('pointerdown', (e) => e.stopPropagation());
		if (b !== kbBtn) b.addEventListener('mousedown', (e) => e.preventDefault());
	}

	kbBtn.addEventListener('click', () => {
		if (document.activeElement === kbd && kbd.dataset.soft) {
			kbd.blur();
			delete kbd.dataset.soft;
		} else {
			kbd.dataset.soft = '1';
			kbd.focus({ preventScroll: true });
		}
	});

	zoomIn.addEventListener('click', () => viewport.zoomBy(ZOOM_STEP));
	zoomOut.addEventListener('click', () => viewport.zoomBy(1 / ZOOM_STEP));
	zoomReset.addEventListener('click', () => viewport.resetZoom());
	// Cycles Fill -> Fit -> Auto.
	fillBtn.addEventListener('click', () => {
		const m = MODES[(MODES.indexOf(viewport.mode()) + 1) % MODES.length];
		viewport.setMode(m);
		flash(
			m === 'auto'
				? 'Auto: edge to edge when the shapes are close, else the whole screen'
				: m === 'fill'
					? 'Fill: edge to edge (edges cut off)'
					: 'Fit: the whole screen'
		);
		try {
			localStorage.setItem(MODE_KEY, m);
		} catch {}
	});
	viewport.onChange(() => {
		const z = viewport.zoom();
		zoomReset.title = `Fit to window (${Math.round(z * 100)}%)`;
		zoomOut.disabled = z <= 1.001;
		const m = viewport.mode();
		fillBtn.classList.toggle('on', m !== 'fit');
		fillBtn.dataset.mode = m;
		fillBtn.title = `View: ${m} (tap for ${MODES[(MODES.indexOf(m) + 1) % MODES.length]})`;
	});

	let hintTimer: ReturnType<typeof setTimeout> | undefined;
	function flash(text: string) {
		hint.textContent = text;
		hint.classList.add('show');
		clearTimeout(hintTimer);
		hintTimer = setTimeout(() => hint.classList.remove('show'), 4000);
	}

	// Fullscreen: the standard API, Safari's prefixed one (iPad), and on an iPhone,
	// which has neither for pages, a pointer to Add to Home Screen.
	const doc = document as Document & WebkitDocument;
	const root = document.documentElement as HTMLElement & WebkitElement;
	const fsEnabled = () =>
		!!(document.fullscreenEnabled || doc.webkitFullscreenEnabled);
	const fsElement = () =>
		document.fullscreenElement || doc.webkitFullscreenElement || null;
	const standalone =
		matchMedia('(display-mode: standalone)').matches ||
		matchMedia('(display-mode: fullscreen)').matches ||
		(navigator as Navigator & { standalone?: boolean }).standalone === true;
	if (!fsEnabled() && (!touch || standalone)) fsBtn.style.display = 'none';
	fsBtn.addEventListener('click', () => {
		if (!fsEnabled()) {
			flash(
				'For full screen, use Share → Add to Home Screen, then open voxl from the Home Screen.'
			);
			return;
		}
		if (fsElement()) {
			if (document.exitFullscreen) document.exitFullscreen().catch(() => {});
			else doc.webkitExitFullscreen?.();
		} else if (root.requestFullscreen)
			root.requestFullscreen({ navigationUI: 'hide' }).catch(() => {});
		else root.webkitRequestFullscreen?.();
	});
	// In fullscreen, capture browser shortcuts (Cmd/Ctrl+W, T, L, ...) for the desktop.
	// Keyboard Lock is Chromium-only and only allowed for the top-level page (an
	// embedding page has to lock on our behalf); hold Esc to leave fullscreen.
	const onFs = () => {
		const kl = navigator.keyboard;
		const on = !!fsElement();
		if (on) kl?.lock().catch(() => {});
		else kl?.unlock();
		fsBtn.classList.toggle('on', on);
		document.documentElement.classList.toggle('fullscreen', on);
		viewport.layout();
	};
	document.addEventListener('fullscreenchange', onFs);
	document.addEventListener('webkitfullscreenchange', onFs);
	// Keyboard shortcuts for the view (they never reach the desktop): Ctrl+Alt+=/-/0
	// zoom, Ctrl+Alt+F fullscreen.
	window.addEventListener(
		'keydown',
		(e) => {
			if (!e.ctrlKey || !e.altKey || e.metaKey) return;
			const k = e.code;
			if (k === 'Equal' || k === 'NumpadAdd') viewport.zoomBy(ZOOM_STEP);
			else if (k === 'Minus' || k === 'NumpadSubtract')
				viewport.zoomBy(1 / ZOOM_STEP);
			else if (k === 'Digit0' || k === 'Numpad0') viewport.resetZoom();
			else if (k === 'KeyF') fsBtn.click();
			else return;
			e.preventDefault();
			e.stopImmediatePropagation();
		},
		true
	);
}
