// Presenting decoded frames (viewport.ts sizes and places them).
import type { Stripe, StripeFrame } from '../protocol';
import type { App } from './app';

interface StripeOut {
	video: HTMLVideoElement;
	writer: WritableStreamDefaultWriter<VideoFrame>;
	writing: boolean;
	next: VideoFrame | null;
}

/**
 * Prefer handing decoded frames to a <video> via MediaStreamTrackGenerator: the
 * compositor scans YUV out directly instead of rasterizing into a 2D canvas. One
 * <video> per encoded stripe, stacked; the worker hands over every stripe of a frame
 * in one message so they are written to their tracks together.
 */
// At most one write in flight per track; a newer frame replaces a waiting one.
function write(o: StripeOut, frame: VideoFrame) {
	if (o.writing) {
		o.next?.close();
		o.next = frame;
		return;
	}
	o.writing = true;
	o.writer
		.write(frame)
		.catch(() => {})
		.finally(() => {
			o.writing = false;
			const next = o.next;
			if (next) {
				o.next = null;
				write(o, next);
			}
		});
}

export function createVideo(
	app: App,
	canvas: HTMLCanvasElement,
	useVideo: boolean
) {
	let outs: StripeOut[] = [];
	let stripes: Stripe[] = [[0, 1]];
	if (useVideo) canvas.style.display = 'none';

	function build(list: Stripe[]) {
		stripes = list;
		for (const o of outs) o.video.remove();
		outs = [];
		if (!useVideo) return;
		for (const [y, h] of list) {
			const gen = new MediaStreamTrackGenerator({ kind: 'video' });
			const video = document.createElement('video');
			video.className = 'stripe';
			video.muted = true;
			video.autoplay = true;
			video.playsInline = true;
			video.disablePictureInPicture = true;
			video.style.top = `${(y / app.desk.h) * 100}%`;
			video.style.height = `${(h / app.desk.h) * 100}%`;
			video.srcObject = new MediaStream([gen]);
			app.surface.appendChild(video);
			video.play().catch(() => {});
			outs.push({
				video,
				writer: gen.writable.getWriter(),
				writing: false,
				next: null,
			});
		}
	}

	function present(parts: StripeFrame[]) {
		for (const p of parts) {
			const o = outs[p.i];
			if (o) write(o, p.frame);
			else p.frame.close();
		}
	}

	return { build, present, stripes: () => stripes };
}
