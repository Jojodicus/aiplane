/**
 * Recorded samples as the gateway's transcription paths want them: 16 kHz
 * mono 16-bit PCM in a canonical 44-byte WAV, the one format its VAD
 * (`aiplane_features::server::vad`) trims and measures. Used by the SPA's
 * voice composer and the embed widget alike.
 */

export const TARGET_RATE = 16000;

/** Linear resample of `samples` from `fromRate` to 16 kHz. */
export function resampleTo16k(samples: Float32Array, fromRate: number): Float32Array {
	if (fromRate === TARGET_RATE) return samples;
	const ratio = fromRate / TARGET_RATE;
	const outLen = Math.floor(samples.length / ratio);
	const out = new Float32Array(outLen);
	for (let i = 0; i < outLen; i++) {
		const src = i * ratio;
		const lo = Math.floor(src);
		const hi = Math.min(lo + 1, samples.length - 1);
		const frac = src - lo;
		out[i] = samples[lo]! * (1 - frac) + samples[hi]! * frac;
	}
	return out;
}

/** Pack samples in -1..1 at 16 kHz into a WAV file's bytes. */
export function encodeWavBytes(samples: Float32Array): ArrayBuffer {
	const dataSize = samples.length * 2;
	const buf = new ArrayBuffer(44 + dataSize);
	const view = new DataView(buf);
	const writeStr = (offset: number, s: string): void => {
		for (let i = 0; i < s.length; i++) view.setUint8(offset + i, s.charCodeAt(i));
	};
	writeStr(0, 'RIFF');
	view.setUint32(4, 36 + dataSize, true);
	writeStr(8, 'WAVE');
	writeStr(12, 'fmt ');
	view.setUint32(16, 16, true);
	view.setUint16(20, 1, true);
	view.setUint16(22, 1, true);
	view.setUint32(24, TARGET_RATE, true);
	view.setUint32(28, TARGET_RATE * 2, true);
	view.setUint16(32, 2, true);
	view.setUint16(34, 16, true);
	writeStr(36, 'data');
	view.setUint32(40, dataSize, true);
	let offset = 44;
	for (let i = 0; i < samples.length; i++) {
		const s = Math.max(-1, Math.min(1, samples[i]!));
		view.setInt16(offset, s < 0 ? s * 0x8000 : s * 0x7fff, true);
		offset += 2;
	}
	return buf;
}

/** Concatenate recorded chunks captured at `captureRate` into one 16 kHz WAV. */
export function chunksToWav(chunks: readonly Float32Array[], captureRate: number): ArrayBuffer {
	let total = 0;
	for (const c of chunks) total += c.length;
	const flat = new Float32Array(total);
	let off = 0;
	for (const c of chunks) {
		flat.set(c, off);
		off += c.length;
	}
	return encodeWavBytes(resampleTo16k(flat, captureRate));
}
