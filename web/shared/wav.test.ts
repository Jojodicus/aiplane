import assert from 'node:assert/strict';
import test from 'node:test';
import { TARGET_RATE, chunksToWav, encodeWavBytes, resampleTo16k } from './wav.ts';

test('a WAV carries a 16 kHz mono 16-bit header and clamped samples', () => {
	const view = new DataView(encodeWavBytes(new Float32Array([0, 1, -1, 2])));
	const text = (at: number) => String.fromCharCode(...[0, 1, 2, 3].map((i) => view.getUint8(at + i)));
	assert.equal(text(0), 'RIFF');
	assert.equal(text(8), 'WAVE');
	assert.equal(view.getUint16(22, true), 1, 'mono');
	assert.equal(view.getUint32(24, true), TARGET_RATE);
	assert.equal(view.getUint16(34, true), 16);
	assert.equal(view.getUint32(40, true), 8);
	assert.deepEqual([44, 46, 48, 50].map((at) => view.getInt16(at, true)), [0, 0x7fff, -0x8000, 0x7fff]);
});

test('48 kHz chunks come out as a third as many 16 kHz samples', () => {
	assert.equal(resampleTo16k(new Float32Array(480), 48000).length, 160);
	const wav = chunksToWav([new Float32Array(240), new Float32Array(240)], 48000);
	assert.equal(wav.byteLength, 44 + 160 * 2);
});
