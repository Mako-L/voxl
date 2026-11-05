// Where the desktop sits on the page: fitted to the window, zoomed and panned by the
// viewer, and kept clear of the on-screen keyboard and the extra-keys bar.
//
// The visible area is the visual viewport (what is left above a phone's keyboard)
// minus the extra-keys bar. While the keyboard is up the desktop keeps the size it
// had before (it doesn't shrink into the strip above the keyboard); instead the view
// is panned so the spot the viewer last tapped, where they are typing, stays in sight.
import type { Stripe } from '../protocol';
import type { App } from './app';

const MAX_ZOOM = 8;
/** Keep the typing spot this far (CSS px) from the visible area's edges. */
const FOCUS_MARGIN = 48;
/** Mouse within this many CSS px of an edge pans a zoomed view that way. */
const EDGE_PX = 28;
const EDGE_SPEED = 14;
/** Auto: fill when that cuts off at most this much (as a ratio of sizes), else fit. */
const AUTO_FILL_MAX = 1.15;

/** fit: all of it, with bars; fill: edge to edge, edges cut off; auto: fill when the
 * shapes are close, fit when they aren't. */
export type FitMode = 'fit' | 'fill' | 'auto';

const gcd = (a: number, b: number): number => (b ? gcd(b, a % b) : a);

export interface Viewport {
	layout(): void;
	/** Zoom by `factor` keeping the point under client (cx, cy) in place. */
	zoomAt(factor: number, cx: number, cy: number): void;
	/** Zoom around the middle of the visible area. */
	zoomBy(factor: number): void;
	resetZoom(): void;
	/** Off: no zooming at all (pinch, wheel, buttons), back to the fitted view. */
	setZoomable(on: boolean): void;
	setMode(m: FitMode): void;
	mode(): FitMode;
	/** Whether the view is filling (edge to edge) right now. */
	filling(): boolean;
	/** Whether the desktop is bigger than the visible area (so it can be panned). */
	canPan(): boolean;
	/** Pan by CSS px; returns what couldn't be panned (the view hit an edge). */
	panBy(dx: number, dy: number): [number, number];
	/** The viewer clicked or tapped here (desktop px): where they will be typing. */
	setFocus(x: number, y: number): void;
	/** Height (CSS px) of the extra-keys bar at the bottom, 0 when hidden. */
	setBarHeight(h: number): void;
	zoom(): number;
	/** Whether the on-screen keyboard is up. */
	keyboardOpen(): boolean;
	/** Notified after every layout. */
	onChange(f: () => void): void;
}

export function createViewport(
	app: App,
	stripes: () => Stripe[],
	onLayout: () => void
): Viewport {
	const { surface, kbd } = app;
	const vv = window.visualViewport;
	const vvEl = document.getElementById('vv');
	let zoom = 1;
	// Surface's top left relative to the visible area's, in CSS px.
	let pan = { x: 0, y: 0 };
	let barH = 0;
	let mode: FitMode = 'fit';
	let zoomable = true;
	let fill = false;
	let big = false;
	// Visible height without the keyboard (the layout viewport's).
	let fullH = vv ? vv.height : window.innerHeight;
	let lastW = vv ? vv.width : window.innerWidth;
	let kbOpen = false;
	let kbSeen = false;
	let focus: { x: number; y: number } | null = null;
	let wantFocus = false;
	const listeners: (() => void)[] = [];

	const softKeyboard = () =>
		document.activeElement === kbd && !!kbd.dataset.soft;

	// Some Android keyboards (Samsung Keyboard's toolbar strip) draw above the top
	// they report, covering the bottom of the page. Keep this much (CSS px) clear above
	// an open keyboard; ?kblift=N overrides it.
	// Not "Android" in the user agent: on a tablet or a Fold's inner screen browsers ask
	// for desktop sites and claim to be Linux. Any touch device but an iPhone or iPad
	// (whose keyboards report their top correctly) gets the lift.
	const liftParam = new URLSearchParams(location.search).get('kblift');
	const ua = navigator.userAgent;
	const apple =
		/iPhone|iPad|iPod/.test(ua) ||
		(/Macintosh/.test(ua) && navigator.maxTouchPoints > 1);
	const touchDevice = matchMedia('(pointer: coarse)').matches;
	const kbLift =
		liftParam !== null && Number.isFinite(Number(liftParam))
			? Math.max(0, Number(liftParam))
			: touchDevice && !apple
				? 36
				: 0;

	function area() {
		const lift = kbOpen ? kbLift : 0;
		return {
			x: vv ? vv.offsetLeft : 0,
			y: vv ? vv.offsetTop : 0,
			w: vv ? vv.width : window.innerWidth,
			h: Math.max(1, (vv ? vv.height : window.innerHeight) - barH - lift),
		};
	}

	function updateKeyboard() {
		const h = vv ? vv.height : window.innerHeight;
		const w = vv ? vv.width : window.innerWidth;
		// The tallest the visible area has been at this width: browsers differ in
		// whether the page's own height (innerHeight) shrinks with the keyboard (Chrome
		// on Android does, on some devices), so only a visible area well short of that
		// means a keyboard is up. Rotating or folding (a new width) starts over.
		if (Math.abs(w - lastW) > 1) {
			lastW = w;
			fullH = h;
		}
		fullH = Math.max(fullH, h);
		const shrunk = !!vv && h < fullH * 0.85;
		// Up for our textarea (Android can bring it back on its own): that's typing.
		if (shrunk && document.activeElement === kbd && !kbd.dataset.soft)
			kbd.dataset.soft = '1';
		const soft = softKeyboard();
		if (soft && shrunk) kbSeen = true;
		// Android's back button hides the keyboard without blurring the textarea.
		if (soft && kbSeen && !shrunk) {
			kbSeen = false;
			delete kbd.dataset.soft;
			kbd.blur();
		}
		const open = shrunk;
		if (open && !kbOpen) wantFocus = true;
		kbOpen = open;
		document.documentElement.classList.toggle('kb-open', kbOpen);
	}

	function layout() {
		updateKeyboard();
		const a = area();
		if (vvEl) {
			vvEl.style.left = `${a.x}px`;
			vvEl.style.top = `${a.y}px`;
			vvEl.style.width = `${a.w}px`;
			vvEl.style.height = `${a.h + barH}px`;
		}
		const { w: dw, h: dh } = app.desk;
		if (!dw) return;
		// Fit into the area without the keyboard, so it popping up doesn't shrink the desktop.
		const fitH = kbOpen ? Math.max(a.h, fullH) : a.h;
		const inside = Math.min(a.w / dw, fitH / dh),
			cover = Math.max(a.w / dw, fitH / dh);
		fill =
			mode === 'fill' || (mode === 'auto' && cover <= inside * AUTO_FILL_MAX);
		const fit = (fill ? cover : inside) || 1;
		const dpr = window.devicePixelRatio || 1;
		// Snap the height so every stripe boundary lands on a whole device pixel;
		// otherwise the stacked stripe layers show hairline seams.
		const q = dh / stripes().reduce((g, [y]) => gcd(g, y), dh);
		const devH = Math.max(q, Math.floor((fit * zoom * dh * dpr) / q) * q);
		app.view.scale = devH / dpr / dh;
		const sw = dw * app.view.scale,
			sh = dh * app.view.scale;
		if (wantFocus && focus) {
			wantFocus = false;
			const fy = focus.y * app.view.scale;
			const top = pan.y + fy;
			if (top < FOCUS_MARGIN || top > a.h - FOCUS_MARGIN)
				pan.y = a.h * 0.45 - fy;
			const fx = focus.x * app.view.scale;
			const left = pan.x + fx;
			if (left < FOCUS_MARGIN || left > a.w - FOCUS_MARGIN)
				pan.x = a.w / 2 - fx;
		}
		wantFocus = false;
		pan.x = sw <= a.w ? (a.w - sw) / 2 : Math.min(0, Math.max(a.w - sw, pan.x));
		pan.y = sh <= a.h ? (a.h - sh) / 2 : Math.min(0, Math.max(a.h - sh, pan.y));
		big = sw > a.w + 1 || sh > a.h + 1;
		document.documentElement.classList.toggle('fill', fill);
		surface.style.width = `${sw}px`;
		surface.style.height = `${sh}px`;
		surface.style.left = `${a.x + pan.x}px`;
		surface.style.top = `${a.y + pan.y}px`;
		document.documentElement.classList.toggle('zoomed', zoom > 1.001);
		onLayout();
		for (const f of listeners) f();
	}

	function zoomAt(factor: number, cx: number, cy: number) {
		const a = area();
		const old = app.view.scale;
		if (!zoomable) return;
		const z = Math.min(MAX_ZOOM, Math.max(1, zoom * factor));
		if (z === zoom || !old) return;
		// Desktop point under (cx, cy) stays there.
		const px = (cx - a.x - pan.x) / old,
			py = (cy - a.y - pan.y) / old;
		zoom = z;
		layout();
		const s = app.view.scale;
		pan.x = cx - a.x - px * s;
		pan.y = cy - a.y - py * s;
		layout();
	}

	function panBy(dx: number, dy: number): [number, number] {
		const before = { ...pan };
		pan.x += dx;
		pan.y += dy;
		layout();
		return [dx - (pan.x - before.x), dy - (pan.y - before.y)];
	}

	// A zoomed view follows a mouse held near its edges.
	let edge = { x: 0, y: 0 },
		edgeRaf = 0;
	function edgeStep() {
		edgeRaf = 0;
		if (zoom <= 1 || (!edge.x && !edge.y)) return;
		const [rx, ry] = panBy(-edge.x * EDGE_SPEED, -edge.y * EDGE_SPEED);
		if (rx !== -edge.x * EDGE_SPEED || ry !== -edge.y * EDGE_SPEED)
			edgeRaf = requestAnimationFrame(edgeStep);
	}
	document.addEventListener('pointermove', (e) => {
		if (e.pointerType !== 'mouse' || zoom <= 1) return;
		const a = area();
		const x = e.clientX - a.x,
			y = e.clientY - a.y;
		edge = {
			x: x < EDGE_PX ? -1 : x > a.w - EDGE_PX ? 1 : 0,
			y: y < EDGE_PX ? -1 : y > a.h - EDGE_PX ? 1 : 0,
		};
		if ((edge.x || edge.y) && !edgeRaf)
			edgeRaf = requestAnimationFrame(edgeStep);
	});
	document.addEventListener('pointerleave', () => (edge = { x: 0, y: 0 }));

	let raf = 0;
	const schedule = () => {
		if (!raf)
			raf = requestAnimationFrame(() => {
				raf = 0;
				layout();
			});
	};
	vv?.addEventListener('resize', schedule);
	vv?.addEventListener('scroll', schedule);
	window.addEventListener('resize', schedule);
	kbd.addEventListener('focus', schedule);
	kbd.addEventListener('blur', () => {
		kbSeen = false;
		schedule();
	});

	return {
		layout,
		zoomAt,
		zoomBy(factor) {
			const a = area();
			zoomAt(factor, a.x + a.w / 2, a.y + a.h / 2);
		},
		setMode(m) {
			if (m === mode) return;
			mode = m;
			zoom = 1;
			// Centred: the parts cut off are shared out evenly.
			pan = { x: -1e6, y: -1e6 };
			layout();
			pan = {
				x: (area().w - surface.offsetWidth) / 2,
				y: (area().h - surface.offsetHeight) / 2,
			};
			layout();
		},
		mode: () => mode,
		filling: () => fill,
		canPan: () => big,
		resetZoom() {
			zoom = 1;
			layout();
		},
		setZoomable(on) {
			if (on === zoomable) return;
			zoomable = on;
			if (!on && zoom !== 1) {
				zoom = 1;
				layout();
			}
		},
		panBy,
		setFocus(x, y) {
			focus = { x, y };
			if (kbOpen) {
				wantFocus = true;
				schedule();
			}
		},
		setBarHeight(h) {
			if (h === barH) return;
			barH = h;
			wantFocus = true;
			schedule();
		},
		zoom: () => zoom,
		keyboardOpen: () => kbOpen,
		onChange(f) {
			listeners.push(f);
		},
	};
}
