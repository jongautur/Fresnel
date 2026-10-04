// Saving the active-test settings from more than one card.
import { api } from "../api/tauri";
import type { TestSettings } from "../types/pointTests";

/** Tell open views that the active-test settings (incl. the iperf3 server) changed. */
export const TEST_SETTINGS_CHANGED = "fresnel:test-settings-changed";

/**
 * Save only some fields: re-read the stored settings first, so two cards
 * editing different parts of the same file don't undo each other.
 */
export async function patchTestSettings(patch: Partial<TestSettings>): Promise<TestSettings> {
  const current = await api.getTestSettings();
  const saved = await api.saveTestSettings({ ...current, ...patch });
  window.dispatchEvent(new CustomEvent(TEST_SETTINGS_CHANGED));
  return saved;
}
