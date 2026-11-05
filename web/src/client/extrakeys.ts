// Extra keys for phones and tablets: the keys an on-screen keyboard lacks (Esc, Tab,
// Ctrl, Alt, Command, arrows, Home/End, Page Up/Down, F1-F12, a few symbols), in a
// bar above the keyboard.
//
// Modifiers are sticky: tap once for the next key (including one typed on the
// on-screen keyboard), twice to lock, again to release. Other keys repeat while held.
// The bar never takes focus, so the keyboard stays up while you use it.
import { cmd } from '../protocol';
import type { App } from './app';
import type { Viewport } from './viewport';

type Key =
	| { label: string; ks: number; mod?: boolean; wide?: boolean; title?: string }
	| { label: string; action: 'fn' | 'kbd'; title?: string };

const XK = {
	Esc: 0xff1b,
	Tab: 0xff09,
	Ctrl: 0xffe3,
	Alt: 0xffe9,
	Super: 0xffeb,
	Shift: 0xffe1,
	Left: 0xff51,
	Up: 0xff52,
	Right: 0xff53,
	Down: 0xff54,
	Home: 0xff50,
	End: 0xff57,
	PgUp: 0xff55,
	PgDn: 0xff56,
	Del: 0xffff,
	Ins: 0xff63,
	Enter: 0xff0d,
	Bksp: 0xff08,
};
const F = (n: number) => 0xffbd + n;
const ch = (c: string) => c.codePointAt(0)!;

const KBD_ICON =
	'<svg class="i" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="M6 9H6.01M10 9H10.01M14 9H14.01M18 9H18.01M8 13H16M4 5H20C21.1 5 22 5.9 22 7V15C22 16.1 21.1 17 20 17H4C2.9 17 2 16.1 2 15V7C2 5.9 2.9 5 4 5ZM10 20L12 22L14 20" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>';

const REPEAT_DELAY = 400;
const REPEAT_EVERY = 50;

export function setupExtraKeys(app: App, viewport: Viewport) {
	const { kbd, send } = app;
	const touch = matchMedia('(pointer: coarse)').matches;
	const host = document.getElementById('vv') ?? document.body;
	const bar = document.createElement('div');
	bar.id = 'xkeys';
	bar.setAttribute('role', 'toolbar');
	bar.setAttribute('aria-label', 'Extra keys');
	host.appendChild(bar);

	let page = 0;
	let pinned = false;
	// Off for a simulated phone, which has its own keyboard and no use for these.
	let enabled = true;
	// Sticky modifiers: keysym -> once (next key) or locked.
	const mods = new Map<number, 'once' | 'lock'>();
	const modButtons = new Map<number, HTMLButtonElement[]>();

	const pages = (): Key[][][] => {
		const mac = app.remoteMac;
		const cmdKey: Key = {
			label: mac ? '⌘' : 'Win',
			ks: XK.Super,
			mod: true,
			title: mac ? 'Command' : 'Super / Windows key',
		};
		const altKey: Key = {
			label: mac ? '⌥' : 'Alt',
			ks: XK.Alt,
			mod: true,
			title: mac ? 'Option' : 'Alt',
		};
		const ctrlKey: Key = {
			label: mac ? '⌃' : 'Ctrl',
			ks: XK.Ctrl,
			mod: true,
			title: 'Control',
		};
		const shift: Key = { label: '⇧', ks: XK.Shift, mod: true, title: 'Shift' };
		const kb: Key = {
			label: '⌨',
			action: 'kbd',
			title: 'Show or hide the keyboard',
		};
		const fn: Key = { label: 'Fn', action: 'fn', title: 'More keys' };
		return [
			[
				[
					{ label: 'Esc', ks: XK.Esc },
					{ label: '/', ks: ch('/') },
					{ label: '-', ks: ch('-') },
					{ label: 'Home', ks: XK.Home },
					{ label: '↑', ks: XK.Up, title: 'Up' },
					{ label: 'End', ks: XK.End },
					{ label: 'PgUp', ks: XK.PgUp, title: 'Page Up' },
					cmdKey,
					kb,
				],
				[
					{ label: 'Tab', ks: XK.Tab },
					ctrlKey,
					altKey,
					{ label: '←', ks: XK.Left, title: 'Left' },
					{ label: '↓', ks: XK.Down, title: 'Down' },
					{ label: '→', ks: XK.Right, title: 'Right' },
					{ label: 'PgDn', ks: XK.PgDn, title: 'Page Down' },
					shift,
					fn,
				],
			],
			[
				[
					...[1, 2, 3, 4, 5, 6].map((n) => ({ label: `F${n}`, ks: F(n) })),
					{ label: 'Del', ks: XK.Del, title: 'Forward Delete' },
					{ label: '~', ks: ch('~') },
					kb,
				],
				[
					...[7, 8, 9, 10, 11, 12].map((n) => ({ label: `F${n}`, ks: F(n) })),
					{ label: '|', ks: ch('|') },
					{ label: '`', ks: ch('`') },
					fn,
				],
			],
		];
	};

	/** Modifiers held for the next key, in press order. */
	function active(): number[] {
		return [...mods.keys()];
	}
	/** After a key: one-shot modifiers let go, locked ones stay. */
	function consume() {
		let changed = false;
		for (const [ks, st] of mods)
			if (st === 'once') {
				mods.delete(ks);
				changed = true;
			}
		if (changed) paintMods();
	}
	app.mods = { active, consume };

	function paintMods() {
		for (const [ks, btns] of modButtons)
			for (const b of btns) {
				const st = mods.get(ks);
				b.classList.toggle('on', st === 'once');
				b.classList.toggle('lock', st === 'lock');
				b.setAttribute('aria-pressed', st ? 'true' : 'false');
			}
	}

	function toggleKeyboard() {
		if (document.activeElement === kbd && kbd.dataset.soft) {
			delete kbd.dataset.soft;
			kbd.blur();
			// Keep the bar for its keys once the keyboard is down.
			pinned = true;
		} else {
			kbd.dataset.soft = '1';
			kbd.focus({ preventScroll: true });
		}
		update();
	}

	let repeat: ReturnType<typeof setTimeout> | undefined;
	let held: { ks: number; mods: number[] } | null = null;
	function keyDown(ks: number) {
		keyUp();
		const m = active();
		for (const k of m) send(cmd.key(k, true));
		send(cmd.key(ks, true));
		held = { ks, mods: m };
		navigator.vibrate?.(5);
		repeat = setTimeout(function again() {
			if (!held) return;
			send(cmd.key(held.ks, true));
			repeat = setTimeout(again, REPEAT_EVERY);
		}, REPEAT_DELAY);
	}
	function keyUp() {
		clearTimeout(repeat);
		if (!held) return;
		send(cmd.key(held.ks, false));
		for (let i = held.mods.length - 1; i >= 0; i--)
			send(cmd.key(held.mods[i], false));
		held = null;
		consume();
	}

	function render() {
		bar.textContent = '';
		modButtons.clear();
		for (const row of pages()[page]) {
			const r = document.createElement('div');
			r.className = 'xrow';
			for (const k of row) {
				const b = document.createElement('button');
				b.type = 'button';
				b.className = 'xk';
				if ('action' in k && k.action === 'kbd') b.innerHTML = KBD_ICON;
				else b.textContent = k.label;
				b.tabIndex = -1;
				if (k.title) b.title = k.title;
				b.setAttribute('aria-label', k.title ?? k.label);
				// Never take focus from the textarea: that would close the keyboard.
				for (const ev of ['mousedown', 'touchstart'] as const)
					b.addEventListener(ev, (e) => e.preventDefault(), { passive: false });
				if ('action' in k) {
					b.classList.add('act');
					if (k.action === 'fn') b.classList.toggle('on', page === 1);
					b.addEventListener('pointerdown', (e) => {
						e.preventDefault();
						e.stopPropagation();
						if (k.action === 'fn') {
							page = page ? 0 : 1;
							render();
						} else toggleKeyboard();
					});
				} else if (k.mod) {
					b.classList.add('mod');
					const list = modButtons.get(k.ks) ?? [];
					list.push(b);
					modButtons.set(k.ks, list);
					b.addEventListener('pointerdown', (e) => {
						e.preventDefault();
						e.stopPropagation();
						const st = mods.get(k.ks);
						if (!st) mods.set(k.ks, 'once');
						else if (st === 'once') mods.set(k.ks, 'lock');
						else mods.delete(k.ks);
						navigator.vibrate?.(5);
						paintMods();
					});
				} else {
					b.addEventListener('pointerdown', (e) => {
						e.preventDefault();
						e.stopPropagation();
						try {
							b.setPointerCapture(e.pointerId);
						} catch {}
						b.classList.add('down');
						keyDown(k.ks);
					});
					const end = () => {
						b.classList.remove('down');
						keyUp();
					};
					b.addEventListener('pointerup', end);
					b.addEventListener('pointercancel', end);
					b.addEventListener('lostpointercapture', end);
				}
				r.appendChild(b);
			}
			bar.appendChild(r);
		}
		paintMods();
	}

	let shown = false;
	function update() {
		const soft = document.activeElement === kbd && !!kbd.dataset.soft;
		const show = enabled && touch && !app.readOnly && (soft || pinned);
		if (show !== shown) {
			shown = show;
			bar.classList.toggle('show', show);
			document.documentElement.classList.toggle('xkeys', show);
		}
		const h = show ? bar.offsetHeight : 0;
		document.documentElement.style.setProperty('--xk-h', `${h}px`);
		viewport.setBarHeight(h);
		kbBtn?.classList.toggle('on', pinned);
	}

	const kbBtn = document.getElementById('keys-btn');
	kbBtn?.addEventListener('click', () => {
		pinned = !pinned;
		if (!pinned) mods.clear();
		paintMods();
		update();
	});

	kbd.addEventListener('focus', () => setTimeout(update));
	kbd.addEventListener('blur', () => setTimeout(update));
	viewport.onChange(() => {
		// Android's back button closes the keyboard without a blur; the viewport notices.
		const soft = document.activeElement === kbd && !!kbd.dataset.soft;
		if (shown !== (enabled && touch && !app.readOnly && (soft || pinned)))
			update();
	});
	window.addEventListener('blur', () => {
		keyUp();
	});

	render();
	update();
	return {
		/** After the server's hello: labels for a Mac or another desktop. */
		refresh() {
			render();
			update();
		},
		setEnabled(on: boolean) {
			enabled = on;
			if (!on) {
				mods.clear();
				paintMods();
			}
			update();
		},
		update,
	};
}
