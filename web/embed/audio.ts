/**
 * The widget's microphone and speaker. Recording is raw PCM through an audio
 * worklet the gateway serves (`/api/v0/embed/recorder.js`), encoded as the
 * 16 kHz WAV the transcription path trims (`web/shared/wav.ts`). Playback
 * decodes the answer's audio into a Web Audio buffer, so it needs no `blob:`
 * URL a host page's CSP could refuse, and it only ever starts from an
 * `AudioContext` the visitor's own click unlocked.
 */
import { TARGET_RATE, chunksToWav } from '../shared/wav.ts';

/** Whether this browser can record here at all (a secure page with Web Audio worklets). */
export function canRecord(): boolean {
	return (
		typeof window !== 'undefined' &&
		window.isSecureContext &&
		!!navigator.mediaDevices?.getUserMedia &&
		typeof AudioContext !== 'undefined' &&
		'audioWorklet' in AudioContext.prototype
	);
}

export class MicRecording {
	private readonly chunks: Float32Array[] = [];
	private closed = false;
	private readonly ctx: AudioContext;
	private readonly stream: MediaStream;
	private readonly source: MediaStreamAudioSourceNode;
	private readonly node: AudioWorkletNode;

	private constructor(ctx: AudioContext, stream: MediaStream, source: MediaStreamAudioSourceNode, node: AudioWorkletNode) {
		this.ctx = ctx;
		this.stream = stream;
		this.source = source;
		this.node = node;
		node.port.onmessage = (e: MessageEvent) => {
			if (e.data instanceof Float32Array) this.chunks.push(e.data);
		};
	}

	static async open(workletUrl: string): Promise<MicRecording> {
		const stream = await navigator.mediaDevices.getUserMedia({
			audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: 1, sampleRate: TARGET_RATE }
		});
		const ctx = new AudioContext({ sampleRate: TARGET_RATE });
		try {
			await ctx.audioWorklet.addModule(workletUrl);
		} catch (err) {
			stream.getTracks().forEach((t) => t.stop());
			await ctx.close().catch(() => {});
			throw err;
		}
		const source = ctx.createMediaStreamSource(stream);
		const node = new AudioWorkletNode(ctx, 'pcm-recorder');
		source.connect(node);
		// The worklet only runs on a path to the destination; a muted gain keeps
		// the microphone from playing back.
		const muted = ctx.createGain();
		muted.gain.value = 0;
		node.connect(muted).connect(ctx.destination);
		return new MicRecording(ctx, stream, source, node);
	}

	/** Stop and hand back the WAV; the microphone is released either way. */
	async stop(): Promise<ArrayBuffer> {
		await this.close();
		return chunksToWav(this.chunks, this.ctx.sampleRate);
	}

	/** Stop and forget what was recorded. */
	async abort(): Promise<void> {
		await this.close();
		this.chunks.length = 0;
	}

	private async close(): Promise<void> {
		if (this.closed) return;
		this.closed = true;
		try {
			this.node.port.onmessage = null;
			this.source.disconnect();
			this.node.disconnect();
		} catch {
			// Torn down already.
		}
		this.stream.getTracks().forEach((t) => t.stop());
		await this.ctx.close().catch(() => {});
	}
}

export class Speaker {
	private ctx: AudioContext | null = null;
	private current: AudioBufferSourceNode | null = null;
	onchange: () => void = () => {};

	/** Call from the visitor's click: browsers only let a gesture start audio. */
	unlock(): void {
		this.ctx ??= new AudioContext();
		void this.ctx.resume().catch(() => {});
	}

	get playing(): boolean {
		return this.current !== null;
	}

	async play(audio: ArrayBuffer): Promise<void> {
		if (!this.ctx) return;
		this.stop();
		const buffer = await this.ctx.decodeAudioData(audio);
		const node = this.ctx.createBufferSource();
		node.buffer = buffer;
		node.connect(this.ctx.destination);
		node.onended = () => {
			if (this.current === node) {
				this.current = null;
				this.onchange();
			}
		};
		this.current = node;
		node.start();
		this.onchange();
	}

	stop(): void {
		const node = this.current;
		this.current = null;
		try {
			node?.stop();
		} catch {
			// Already ended.
		}
		if (node) this.onchange();
	}
}
