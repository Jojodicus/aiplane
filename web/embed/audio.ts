/**
 * The widget's speaker. Playback decodes the answer's audio into a Web Audio
 * buffer, so it needs no `blob:` URL a host page's CSP could refuse, and it
 * only ever starts from an `AudioContext` the visitor's own click unlocked.
 * Recording is `web/shared/voice-recorder.ts`, shared with the SPA.
 */
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
