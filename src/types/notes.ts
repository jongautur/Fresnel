// Mirrors fresnel-core database/notes.rs and database/photos.rs (camelCase).

/** The backend's limit for a note (characters). */
export const MAX_NOTE_CHARS = 4000;
export const MAX_CAPTION_CHARS = 500;

export type NoteTarget = { kind: "floor" | "point" | "ap"; id: number };
export type PhotoTarget = { kind: "floor" | "point" | "ap" | "pin"; id: number };

export interface NotePin {
  id: number;
  floorId: number;
  /** Plan pixels, like survey points. */
  x: number;
  y: number;
  text: string;
  category: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface NotePinInput {
  x: number;
  y: number;
  text: string;
  category: string | null;
}

/** Suggested pin categories (stored as free text). */
export const PIN_CATEGORIES: { id: string; label: string }[] = [
  { id: "obstruction", label: "Obstruction" },
  { id: "interference", label: "Interference" },
  { id: "construction", label: "Construction" },
  { id: "access", label: "Access / mounting" },
  { id: "info", label: "Info" },
];

export function pinCategoryLabel(c: string | null): string | null {
  if (!c) return null;
  return PIN_CATEGORIES.find((p) => p.id === c)?.label ?? c;
}

export interface Photo {
  id: number;
  floorId: number;
  target: PhotoTarget;
  /** The original, metadata included. Never shown or put in a report. */
  file: string;
  /** Downscaled JPEG without metadata. */
  reportFile: string;
  thumbFile: string;
  /** The original's size, upright. */
  width: number;
  height: number;
  /** EXIF capture time, `YYYY-MM-DDTHH:MM:SS` plus an offset only if the camera recorded one. */
  takenAt: string | null;
  /** The original's EXIF contains a GPS position. */
  hadGps: boolean;
  caption: string | null;
  inReport: boolean;
  createdAt: string;
}
