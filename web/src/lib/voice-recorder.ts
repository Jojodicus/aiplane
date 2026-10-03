/**
 * In-browser voice recorder for the SPA (issue #22 P2) — the port of the
 * legacy `ui/ts/voice-recorder.ts`: raw PCM via Web Audio + AudioWorklet,
 * resampled to 16 kHz mono, encoded as a canonical 44-byte WAV. PCM rather
 * than MediaRecorder/Opus because the `/api/v0/transcriptions` handler runs
 * the upload through a neural VAD (earshot) that needs raw PCM16 — see
 * `crates/aiplane-features/src/server/vad.rs`; the encoding is `web/shared/wav.ts`.
 *
 * Differences from the legacy module: the worklet URL is an explicit
 * parameter (the SPA serves it at `${base}/pcm-recorder.js`) and the
 * DOM-level meter tap is dropped (the SPA modal animates off CSS state).
 */
import { t } from './i18n.svelte';
import { TARGET_RATE, chunksToWav } from '../../shared/wav.ts';

/** One recording session: AudioContext, mic stream, worklet, chunk buffer. */
class VoiceRecorder {
	private readonly chunks: Float32Array[] = [];
	private readonly captureRate: number;

	constructor(
		private readonly ctx: AudioContext,
		private readonly stream: MediaStream,
		private readonly source: MediaStreamAudioSourceNode,
		private readonly node: AudioWorkletNode
	) {
		this.captureRate = ctx.sampleRate;
		this.node.port.onmessage = (e: MessageEvent) => {
			if (e.data instanceof Float32Array) this.chunks.push(e.data);
		};
	}

	/** Stop capture, tear everything down, return the encoded WAV. */
	async stop(): Promise<Blob> {
		try {
			this.node.port.onmessage = null;
			this.source.disconnect();
			this.node.disconnect();
		} catch {
			// best-effort tear-down
		} finally {
			// Releasing the mic and closing the context must happen even if a
			// disconnect above threw — otherwise the mic stays hot.
			try {
				this.stream.getTracks().forEach((t) => t.stop());
			} catch {
				/* best-effort */
			}
			try {
				await this.ctx.close();
			} catch {
				/* best-effort */
			}
		}
		if (this.chunks.length === 0) return new Blob([], { type: 'audio/wav' });
		return new Blob([chunksToWav(this.chunks, this.captureRate)], { type: 'audio/wav' });
	}
}

export type { VoiceRecorder };

/** Capability check — runs at click time so a fresh mount re-verifies. */
export const recordingUnavailableReason = (): string | null => {
	if (!navigator.mediaDevices || !window.isSecureContext) {
		return t('voice-mic-insecure-context');
	}
	if (!(window.AudioContext && 'audioWorklet' in AudioContext.prototype)) {
		return t('voice-mic-no-worklet');
	}
	return null;
};

/** Start a recording session against the worklet at `workletUrl`. */
export const startRecording = async (workletUrl: string): Promise<VoiceRecorder> => {
	const stream = await navigator.mediaDevices.getUserMedia({
		audio: {
			echoCancellation: true,
			noiseSuppression: true,
			autoGainControl: true,
			channelCount: 1,
			sampleRate: TARGET_RATE
		}
	});
	const ctx = new AudioContext({ sampleRate: TARGET_RATE });
	try {
		await ctx.audioWorklet.addModule(workletUrl);
	} catch (err) {
		await ctx.close().catch(() => {});
		stream.getTracks().forEach((t) => t.stop());
		throw err;
	}
	const source = ctx.createMediaStreamSource(stream);
	const node = new AudioWorkletNode(ctx, 'pcm-recorder');
	// The processor only runs when there's an active path to the destination;
	// route it through a muted gain so `process()` is scheduled without
	// playing the mic back.
	source.connect(node);
	const muted = ctx.createGain();
	muted.gain.value = 0;
	node.connect(muted).connect(ctx.destination);
	return new VoiceRecorder(ctx, stream, source, node);
};

export const recordingErrorMessage = (err: unknown): string => {
	const name = (err instanceof Error && err.name) || 'Error';
	if (name === 'NotAllowedError' || name === 'PermissionDeniedError') {
		return t('voice-mic-denied');
	}
	if (name === 'NotFoundError' || name === 'DevicesNotFoundError') {
		return t('voice-mic-not-found');
	}
	if (name === 'NotReadableError') {
		return t('voice-mic-busy');
	}
	const message = err instanceof Error ? err.message : String(err);
	return t('voice-mic-error', { error: message });
};
