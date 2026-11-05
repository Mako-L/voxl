// Touch: tap = click, drag = left-drag, long press = right click, two fingers =
// scroll (content follows the fingers), pinch = zoom the view (and pan it with the
// fingers). While zoomed in, a two-finger drag pans the view first and scrolls the
// desktop once the view reaches its edge.
import { cmd } from '../protocol';
import type { App } from './app';

const SLOP = 10;
const LONG_PRESS = 550;
/** A tap this soon (GTK's and Qt's double-click time) and this close (CSS px) after
 * the last one clicks the same spot, so double-taps double-click even though the
 * finger lands a little off. */
const MULTI_TAP_MS = 400;
const MULTI_TAP_PX = 24;
/** Fingers spreading or pinching by this much (ratio) make a two-finger touch a zoom. */
const PINCH_START = 0.08;

interface Hooks {
	/** Move the remote pointer to a client (CSS pixel) position. */
	moveTo(x: number, y: number): void;
	/** A tap (click) at a client position. */
	tapAt(x: number, y: number): void;
	queueScroll(dx: number, dy: number): void;
	deskPerPx(): number;
	zoomAt(factor: number, cx: number, cy: number): void;
	/** Pan the view; returns what it couldn't pan. */
	panBy(dx: number, dy: number): [number, number];
	zoomed(): boolean;
}

export function createTouch(app: App, hooks: Hooks) {
	const { send } = app;
	const pts = new Map<number, { x: number; y: number }>();
	let mode: 'pending' | 'drag' | 'two' | 'scroll' | 'pinch' | 'done' | null =
		null;
	let startDist = 0,
		lastDist = 0;
	let twoStart: [number, number] = [0, 0];
	const dist = () => {
		const [a, b] = [...pts.values()];
		return a && b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
	};
	let start = { x: 0, y: 0 };
	let timer: ReturnType<typeof setTimeout> | undefined;
	let lastMid: [number, number] = [0, 0];
	let lastTap = { x: 0, y: 0, t: -Infinity };
	const mid = (): [number, number] => {
		let x = 0,
			y = 0;
		for (const p of pts.values()) {
			x += p.x;
			y += p.y;
		}
		return [x / pts.size, y / pts.size];
	};
	const click = (button: number) => {
		send(cmd.button(button, true));
		send(cmd.button(button, false));
	};

	function down(e: PointerEvent) {
		try {
			app.surface.setPointerCapture(e.pointerId);
		} catch {}
		pts.set(e.pointerId, { x: e.clientX, y: e.clientY });
		if (pts.size === 1) {
			mode = 'pending';
			start = { x: e.clientX, y: e.clientY };
			timer = setTimeout(() => {
				if (mode !== 'pending') return;
				hooks.moveTo(start.x, start.y);
				click(3);
				mode = 'done';
				navigator.vibrate?.(10);
			}, LONG_PRESS);
		} else if (pts.size === 2) {
			clearTimeout(timer);
			if (mode === 'drag') send(cmd.button(1, false));
			// Undecided until the fingers either spread/pinch or move together.
			mode = 'two';
			lastMid = mid();
			twoStart = lastMid;
			startDist = lastDist = dist();
		}
	}
	function move(e: PointerEvent) {
		const p = pts.get(e.pointerId);
		if (!p) return;
		p.x = e.clientX;
		p.y = e.clientY;
		if (mode === 'pending' && Math.hypot(p.x - start.x, p.y - start.y) > SLOP) {
			clearTimeout(timer);
			mode = 'drag';
			hooks.moveTo(start.x, start.y);
			send(cmd.button(1, true));
		}
		if (mode === 'drag') hooks.moveTo(p.x, p.y);
		else if (mode === 'two' && pts.size >= 2) {
			const d = dist(),
				m = mid();
			if (startDist && Math.abs(d / startDist - 1) > PINCH_START) {
				mode = 'pinch';
				lastDist = d;
				lastMid = m;
			} else if (Math.hypot(m[0] - twoStart[0], m[1] - twoStart[1]) > SLOP) {
				mode = 'scroll';
			}
		}
		if (mode === 'pinch' && pts.size >= 2) {
			const d = dist(),
				m = mid();
			if (lastDist && d) hooks.zoomAt(d / lastDist, m[0], m[1]);
			hooks.panBy(m[0] - lastMid[0], m[1] - lastMid[1]);
			lastDist = d;
			lastMid = m;
		} else if (mode === 'scroll' && pts.size >= 2) {
			const m = mid();
			let dx = m[0] - lastMid[0],
				dy = m[1] - lastMid[1];
			// Zoomed in: move the view first, scroll the desktop with what's left.
			if (hooks.zoomed()) [dx, dy] = hooks.panBy(dx, dy);
			const k = hooks.deskPerPx();
			if (dx || dy) hooks.queueScroll(-dx * k, -dy * k);
			lastMid = m;
		}
	}
	function up(e: PointerEvent) {
		if (!pts.delete(e.pointerId)) return;
		if (mode === 'pending') {
			clearTimeout(timer);
			const now = performance.now();
			const at =
				now - lastTap.t < MULTI_TAP_MS &&
				Math.hypot(start.x - lastTap.x, start.y - lastTap.y) < MULTI_TAP_PX
					? lastTap
					: start;
			lastTap = { x: at.x, y: at.y, t: now };
			hooks.moveTo(at.x, at.y);
			hooks.tapAt(at.x, at.y);
			click(1);
			mode = 'done';
		} else if (mode === 'drag') {
			send(cmd.button(1, false));
			mode = 'done';
		} else if (
			(mode === 'scroll' || mode === 'pinch' || mode === 'two') &&
			pts.size < 2
		)
			mode = 'done';
		if (pts.size === 0) mode = null;
	}
	function cancel(e: PointerEvent) {
		clearTimeout(timer);
		if (mode === 'drag') send(cmd.button(1, false));
		pts.delete(e.pointerId);
		if (pts.size === 0) mode = null;
	}
	return { down, move, up, cancel };
}
