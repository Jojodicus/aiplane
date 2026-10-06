/**
 * The SPA's side of the shared microphone (`web/shared/voice-recorder.ts`,
 * also the embed widget's): the recording as the `Blob` the transcription
 * uploads take, and the failures worded in the user's language. The worklet
 * is served at `/pcm-recorder.js` (`web/static/`).
 */
import { t } from './i18n.svelte';
import { MicRecording, recordingBlocker } from '../../shared/voice-recorder.ts';

/** One recording session; `stop` hands back the WAV, empty when nothing was captured. */
export interface VoiceRecorder {
	stop(): Promise<Blob>;
}

/** Capability check — runs at click time so a fresh mount re-verifies. */
export const recordingUnavailableReason = (): string | null => {
	switch (recordingBlocker()) {
		case 'insecure-context':
			return t('voice-mic-insecure-context');
		case 'no-worklet':
			return t('voice-mic-no-worklet');
		default:
			return null;
	}
};

/** Start a recording session against the worklet at `workletUrl`. */
export const startRecording = async (workletUrl: string): Promise<VoiceRecorder> => {
	const recording = await MicRecording.open(workletUrl);
	return {
		stop: async () => {
			const wav = await recording.stop();
			return new Blob(wav ? [wav] : [], { type: 'audio/wav' });
		}
	};
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
