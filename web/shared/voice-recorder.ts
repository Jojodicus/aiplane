/**
 * The microphone, as the SPA's voice composer, dictation and feedback and the
 * embed widget all record it: raw PCM through the `pcm-recorder` audio
 * worklet (`web/static/pcm-recorder.js`, the one copy; the SPA serves it at
 * `/pcm-recorder.js`, the gateway at `/api/v0/embed/recorder.js` for
 * the widget's cross-origin load), encoded as the 16 kHz WAV the
 * transcription paths trim (`wav.ts`). PCM rather than MediaRecorder/Opus
 * because the transcription handlers run the upload through a VAD that needs
 * raw PCM16 (`crates/aiplane-features/src/server/vad.rs`).
 *
 * Framework-free and string-free, so the widget bundle stays small: callers
 * word the failures themselves.
 */
import { TARGET_RATE, chunksToWav } from './wav.ts';

/** Why this page cannot record, if it cannot. */
export type RecordingBlocker = 'insecure-context' | 'no-worklet';

/** The parts of the browser recording needs; `globalThis` in a page. */
export interface RecordingEnv {
	isSecureContext?: boolean;
	navigator?: { mediaDevices?: { getUserMedia?: unknown } };
	AudioContext?: { prototype: object };
}

/** Checked at click time, so a page that changed since mount is judged as it is now. */
export function recordingBlocker(env: RecordingEnv = globalThis as RecordingEnv): RecordingBlocker | null {
	if (!env.isSecureContext || !env.navigator?.mediaDevices?.getUserMedia) return 'insecure-context';
	if (!env.AudioContext || !('audioWorklet' in env.AudioContext.prototype)) return 'no-worklet';
	return null;
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

	/** Ask for the microphone and start recording through the worklet at `workletUrl`. */
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

	/** Stop and hand back the WAV, or `null` when nothing was captured; the microphone is released either way. */
	async stop(): Promise<ArrayBuffer | null> {
		await this.close();
		return this.chunks.length ? chunksToWav(this.chunks, this.ctx.sampleRate) : null;
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
		// Releasing the microphone and closing the context must happen even if
		// a disconnect threw, or the microphone stays hot.
		try {
			this.stream.getTracks().forEach((t) => t.stop());
		} catch {
			// Best effort.
		}
		await this.ctx.close().catch(() => {});
	}
}
