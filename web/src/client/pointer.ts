// Mouse, wheel/trackpad and touch input on the desktop surface.
import { cmd } from '../protocol';
import type { App } from './app';
import { createTouch } from './touch';
import type { Viewport } from './viewport';

/** DOM button -> X button (left, middle, right, back, forward). */
const BUTTONS = [1, 2, 3, 8, 9];

export function setupPointer(
	app: App,
	viewport: Viewport,
	onInteract: () => void
) {
	const { surface, send } = app;

	function toDesk(p: { clientX: number; clientY: number }): [number, number] {
		const r = surface.getBoundingClientRect();
		const x = Math.round(((p.clientX - r.left) * app.desk.w) / r.width);
		const y = Math.round(((p.clientY - r.top) * app.desk.h) / r.height);
		return [
			Math.max(0, Math.min(app.desk.w - 1, x)),
			Math.max(0, Math.min(app.desk.h - 1, y)),
		];
	}
	let lastMove = '';
	function move(p: { clientX: number; clientY: number }, force = false) {
		if (!app.desk.w) return;
		const s = cmd.move(...toDesk(p));
		if (s !== lastMove || force) {
			lastMove = s;
			send(s);
		}
	}

	// Scroll is sent in desktop pixels, batched per frame; the server turns it into
	// smooth (pixel-precise) scrolling, or wheel clicks on X servers that can't.
	let sx = 0,
		sy = 0,
		queued = false;
	function queueScroll(dx: number, dy: number) {
		sx += dx;
		sy += dy;
		if (queued) return;
		queued = true;
		requestAnimationFrame(() => {
			queued = false;
			if (Math.abs(sx) < 0.5 && Math.abs(sy) < 0.5) return;
			send(cmd.scroll(sx, sy));
			sx = 0;
			sy = 0;
		});
	}
	/** Desktop pixels per on-screen pixel. */
	const deskPerPx = () => app.desk.w / surface.getBoundingClientRect().width;

	const touch = createTouch(app, {
		moveTo: (x, y) => move({ clientX: x, clientY: y }, true),
		// A tap is where the viewer will type: keep it in view above the keyboard.
		tapAt: (x, y) => viewport.setFocus(...toDesk({ clientX: x, clientY: y })),
		queueScroll,
		deskPerPx,
		zoomAt: viewport.zoomAt,
		panBy: viewport.panBy,
		zoomed: () => viewport.canPan(),
	});
	// The page itself never scrolls or zooms under the fingers, and taps on the desktop
	// don't move focus off the keyboard's textarea (which would close the keyboard).
	surface.addEventListener('touchstart', (e) => e.preventDefault(), {
		passive: false,
	});

	surface.addEventListener('pointermove', (e) => {
		if (e.pointerType === 'touch') touch.move(e);
		else move(e);
	});
	surface.addEventListener('pointerdown', (e) => {
		e.preventDefault();
		onInteract();
		if (e.pointerType === 'touch') {
			touch.down(e);
			return;
		}
		app.kbd.focus({ preventScroll: true });
		move(e);
		viewport.setFocus(...toDesk(e));
		try {
			surface.setPointerCapture(e.pointerId);
		} catch {}
		send(cmd.button(BUTTONS[e.button] || 1, true));
	});
	surface.addEventListener('pointerup', (e) => {
		e.preventDefault();
		if (e.pointerType === 'touch') {
			touch.up(e);
			return;
		}
		move(e);
		send(cmd.button(BUTTONS[e.button] || 1, false));
	});
	surface.addEventListener('pointercancel', (e) => {
		if (e.pointerType === 'touch') touch.cancel(e);
		else send(cmd.releaseAll);
	});
	surface.addEventListener('contextmenu', (e) => e.preventDefault());
	surface.addEventListener(
		'wheel',
		(e) => {
			e.preventDefault();
			// A trackpad pinch arrives as a wheel event with ctrlKey and fractional
			// deltas: zoom the view. (A real Ctrl+wheel, whole steps, goes to the desktop.)
			if (e.ctrlKey && e.deltaMode === 0 && !Number.isInteger(e.deltaY)) {
				viewport.zoomAt(Math.exp(-e.deltaY / 100), e.clientX, e.clientY);
				return;
			}
			move(e);
			const k =
				(e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 800 : 1) * deskPerPx();
			queueScroll(e.deltaX * k, e.deltaY * k);
		},
		{ passive: false }
	);
}
