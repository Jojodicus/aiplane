// croit AIplane — AudioWorklet PCM recorder processor, the only copy.
//
// Loaded via `audioWorklet.addModule()` by web/shared/voice-recorder.ts. The
// SPA serves it at `${base}/pcm-recorder.js` (web/static is copied verbatim
// by the SvelteKit build); the gateway embeds it and serves it at
// /api/v0/embed/recorder.js under the embed CORS the widget's cross-origin
// load needs. The main-thread side (chunk accumulation + WAV encode) only
// understands this message shape.

class PcmRecorder extends AudioWorkletProcessor {
  process(inputs) {
    const channel = inputs[0] && inputs[0][0];
    if (channel && channel.length) {
      // .slice() because the underlying buffer is reused by the graph on
      // the next quantum — without the copy the main thread would observe
      // overwritten samples.
      this.port.postMessage(channel.slice());
    }
    return true;
  }
}

registerProcessor('pcm-recorder', PcmRecorder);
