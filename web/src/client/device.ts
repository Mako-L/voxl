// The iOS Simulator's hardware (when the server streams it with --app): Home, Lock,
// volume, Action, Siri, screenshot and rotation buttons in a panel by the toolbar,
// and the simulated iPhone following this phone's orientation, so rotating (or
// folding and unfolding) the phone in your hand rotates the one on screen too.
import { cmd } from '../protocol';
import type { App } from './app';
import { el } from './app';

const SYNC_KEY = 'voxl.syncRotation';

interface Btn {
	action: string;
	label: string;
	title: string;
}
const BUTTONS: Btn[] = [
	{ action: 'home', label: 'Home', title: 'Home (swipe up)' },
	{ action: 'lock', label: 'Lock', title: 'Side button: lock / wake' },
	{ action: 'voldown', label: 'Vol −', title: 'Volume down' },
	{ action: 'volup', label: 'Vol +', title: 'Volume up' },
	{ action: 'action', label: 'Action', title: 'Action button' },
	{ action: 'siri', label: 'Siri', title: 'Siri' },
	{
		action: 'screenshot',
		label: 'Shot',
		title: 'Take a screenshot on the device',
	},
	{ action: 'shake', label: 'Shake', title: 'Shake gesture' },
	{ action: 'rotl', label: '⟲', title: 'Rotate left' },
	{ action: 'rotr', label: '⟳', title: 'Rotate right' },
];

/** The simulated iPhone orientation that matches this screen's. */
function orientationAction(): string | null {
	const o = screen.orientation;
	if (!o) return null;
	switch (o.type) {
		case 'portrait-primary':
			return 'portrait';
		case 'portrait-secondary':
			return 'upside-down';
		// Primary landscape is the phone turned counter-clockwise (top to the left),
		// which the Simulator calls Landscape Left.
		case 'landscape-primary':
			return 'landscape-left';
		case 'landscape-secondary':
			return 'landscape-right';
	}
	return null;
}

/** On a Fold, the big inner screen (unfolded) rather than the cover screen. */
const large = () => Math.min(screen.width, screen.height) >= 600;

export function setupDevice(app: App) {
	const btn = el<HTMLButtonElement>('dev-btn');
	const panel = el('devpanel');
	let on = false;
	let sync = true;
	try {
		sync = localStorage.getItem(SYNC_KEY) !== '0';
	} catch {}
	let lastSent: string | null = null;
	let synced = false;

	const send = (action: string) => app.send(cmd.device(action));

	function render() {
		panel.textContent = '';
		for (const b of BUTTONS) {
			const e = document.createElement('button');
			e.type = 'button';
			e.className = 'dk';
			e.textContent = b.label;
			e.title = b.title;
			e.setAttribute('aria-label', b.title);
			e.addEventListener('mousedown', (ev) => ev.preventDefault());
			e.addEventListener('pointerdown', (ev) => ev.stopPropagation());
			e.addEventListener('click', () => {
				navigator.vibrate?.(8);
				send(b.action);
			});
			panel.appendChild(e);
		}
		const s = document.createElement('button');
		s.type = 'button';
		s.className = 'dk wide' + (sync ? ' on' : '');
		s.textContent = sync ? 'Rotation: follows this phone' : 'Rotation: manual';
		s.title = 'Rotate the simulated iPhone when this phone rotates';
		s.setAttribute('aria-pressed', String(sync));
		s.addEventListener('mousedown', (ev) => ev.preventDefault());
		s.addEventListener('pointerdown', (ev) => ev.stopPropagation());
		s.addEventListener('click', () => {
			sync = !sync;
			try {
				localStorage.setItem(SYNC_KEY, sync ? '1' : '0');
			} catch {}
			lastSent = null;
			if (sync) follow();
			render();
		});
		panel.appendChild(s);
	}

	/** Rotate the simulated iPhone to match this screen, if that changed. */
	const touch = matchMedia('(pointer: coarse)').matches;
	function follow() {
		// Only a phone or tablet in the hand has an orientation worth following.
		if (!on || !sync || !touch || app.readOnly) return;
		const a = orientationAction();
		if (!a || a === lastSent) return;
		lastSent = a;
		send(a);
	}

	btn.addEventListener('pointerdown', (e) => e.stopPropagation());
	btn.addEventListener('mousedown', (e) => e.preventDefault());
	btn.addEventListener('click', () => {
		const open = !panel.classList.contains('open');
		panel.classList.toggle('open', open);
		btn.classList.toggle('on', open);
	});
	screen.orientation?.addEventListener('change', follow);
	// Folding or unfolding can change the orientation without a rotation event.
	let t: ReturnType<typeof setTimeout> | undefined;
	window.addEventListener('resize', () => {
		clearTimeout(t);
		t = setTimeout(follow, 300);
	});

	// Folding the phone (seen as the screen shrinking to the cover display's size)
	// folds an iPhone Duo the same way, or locks any other iPhone and wakes it on
	// unfolding: the server decides from the posture.
	let wasLarge = large();
	function onFold() {
		const now = large();
		if (now === wasLarge) return;
		wasLarge = now;
		// The Android app watches the hinge itself, even in the background.
		if (!on || !touch || app.readOnly || / VoxlApp\//.test(navigator.userAgent))
			return;
		send(now ? 'posture-open' : 'posture-closed');
	}
	window.addEventListener('resize', onFold);
	screen.orientation?.addEventListener('change', onFold);

	// For the Android app (android/): the phone's own hardware keys (volume, camera)
	// call this.
	(window as Window & { voxlDevice?: (a: string) => void }).voxlDevice = (
		a
	) => {
		if (on && !app.readOnly) send(a);
	};

	render();
	return {
		/** From each hello: whether the stream is the iOS Simulator. */
		setup(sim: boolean) {
			on = sim && !app.readOnly;
			app.tools.classList.toggle('sim', on);
			if (!on) panel.classList.remove('open');
			// Once per page: a stream restart (a new hello) is no reason to re-send it.
			if (on && !synced) {
				synced = true;
				follow();
			}
		},
	};
}
