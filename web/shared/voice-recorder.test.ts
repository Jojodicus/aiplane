import assert from 'node:assert/strict';
import test from 'node:test';
import { MicRecording, recordingBlocker } from './voice-recorder.ts';

test('recording needs a secure page with a microphone and audio worklets', () => {
	const worklets = { prototype: { audioWorklet: {} } };
	const mic = { mediaDevices: { getUserMedia: () => {} } };
	assert.equal(recordingBlocker({ isSecureContext: true, navigator: mic, AudioContext: worklets }), null);
	assert.equal(recordingBlocker({ isSecureContext: false, navigator: mic, AudioContext: worklets }), 'insecure-context');
	assert.equal(recordingBlocker({ isSecureContext: true, navigator: {}, AudioContext: worklets }), 'insecure-context');
	assert.equal(recordingBlocker({ isSecureContext: true, navigator: mic, AudioContext: { prototype: {} } }), 'no-worklet');
	assert.equal(recordingBlocker({ isSecureContext: true, navigator: mic }), 'no-worklet');
	assert.equal(recordingBlocker(), 'insecure-context', 'node is no browser');
});

// Node has no microphone and no Web Audio, so these stand-ins play the
// browser: they record what happened to them, and the tests read that state.
class FakeTrack {
	stopped = false;
	stop() {
		this.stopped = true;
	}
}

class FakeContext {
	static last: FakeContext;
	readonly sampleRate = 16000;
	readonly modules: string[] = [];
	closed = false;
	readonly destination = {};
	readonly audioWorklet = {
		addModule: async (url: string) => {
			if (FakeContext.failNext) throw new Error('blocked by CSP');
			this.modules.push(url);
		}
	};
	static failNext = false;
	constructor() {
		FakeContext.last = this;
	}
	createMediaStreamSource() {
		return { connect() {}, disconnect() {} };
	}
	createGain() {
		return { gain: { value: 1 }, connect: () => ({}) };
	}
	async close() {
		this.closed = true;
	}
}

class FakeWorkletNode {
	static last: FakeWorkletNode;
	readonly port: { onmessage: ((e: { data: unknown }) => void) | null } = { onmessage: null };
	constructor() {
		FakeWorkletNode.last = this;
	}
	connect() {
		return { connect: () => ({}) };
	}
	disconnect() {}
	emit(samples: number) {
		this.port.onmessage?.({ data: new Float32Array(samples) });
	}
}

function browser(): FakeTrack {
	const track = new FakeTrack();
	Object.defineProperty(globalThis, 'navigator', {
		configurable: true,
		value: { mediaDevices: { getUserMedia: async () => ({ getTracks: () => [track] }) } }
	});
	Object.assign(globalThis, { AudioContext: FakeContext, AudioWorkletNode: FakeWorkletNode });
	FakeContext.failNext = false;
	return track;
}

test('a recording loads the worklet it is given and hands back the samples as one WAV', async () => {
	const track = browser();
	const recording = await MicRecording.open('https://gw.test/api/v0/embed/recorder.js');
	assert.deepEqual(FakeContext.last.modules, ['https://gw.test/api/v0/embed/recorder.js']);
	FakeWorkletNode.last.emit(128);
	FakeWorkletNode.last.emit(128);
	FakeWorkletNode.last.port.onmessage?.({ data: 'not samples' });
	const wav = await recording.stop();
	assert.equal(wav?.byteLength, 44 + 256 * 2);
	assert.ok(track.stopped, 'the microphone is released');
	assert.ok(FakeContext.last.closed);
});

test('a recording that captured nothing, or was aborted, hands back no WAV', async () => {
	browser();
	assert.equal(await (await MicRecording.open('/pcm-recorder.js')).stop(), null);
	const track = browser();
	const aborted = await MicRecording.open('/pcm-recorder.js');
	FakeWorkletNode.last.emit(128);
	await aborted.abort();
	assert.ok(track.stopped);
	assert.equal(await aborted.stop(), null);
});

test('a worklet that cannot load releases the microphone before the error surfaces', async () => {
	const track = browser();
	FakeContext.failNext = true;
	await assert.rejects(MicRecording.open('/pcm-recorder.js'), /blocked by CSP/);
	assert.ok(track.stopped);
	assert.ok(FakeContext.last.closed);
});
