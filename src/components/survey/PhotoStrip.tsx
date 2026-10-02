import { useEffect, useRef, useState, type ChangeEvent } from "react";
import { api, asApiError } from "../../api/tauri";
import type { ApiError } from "../../types/wifi";
import { MAX_CAPTION_CHARS, type Photo, type PhotoTarget } from "../../types/notes";
import { ErrorBanner } from "../ErrorBanner";
import { ConfirmButton } from "../ConfirmButton";
import "../../styles/notes.css";

/** The backend's import limit. */
const MAX_PHOTO_BYTES = 25 * 1024 * 1024;
const ACCEPT = "image/jpeg,image/png,image/webp,.jpg,.jpeg,.png,.webp,.heic,.heif";

const sameTarget = (a: PhotoTarget, b: PhotoTarget) => a.kind === b.kind && a.id === b.id;

/** EXIF time as written (no zone conversion: without an offset the zone is unknown). */
function formatTakenAt(t: string): string {
  const [date, rest = ""] = t.split("T");
  return `${date} ${rest.slice(0, 5)}${rest.length > 8 ? ` (UTC${rest.slice(8)})` : ""}`;
}

/** Bytes from the backend → an object URL, revoked when the component lets go of it. */
function useBlobUrl(load: (() => Promise<ArrayBuffer>) | null, deps: unknown[]): string | null {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    setUrl(null);
    if (!load) return;
    let made: string | null = null;
    let cancelled = false;
    load()
      .then((buf) => {
        if (cancelled) return;
        made = URL.createObjectURL(new Blob([buf], { type: "image/jpeg" }));
        setUrl(made);
      })
      .catch(() => {
        /* the tile shows a placeholder */
      });
    return () => {
      cancelled = true;
      if (made) URL.revokeObjectURL(made);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return url;
}

function Thumb({ photo, onOpen }: { photo: Photo; onOpen: () => void }) {
  const url = useBlobUrl(() => api.photoThumbnail(photo.id), [photo.id]);
  return (
    <button
      type="button"
      className={`photo-thumb ${photo.inReport ? "" : "excluded"}`}
      onClick={onOpen}
      title={[photo.caption, photo.inReport ? null : "Not in report"].filter(Boolean).join(" · ") || "View photo"}
    >
      {url ? <img src={url} alt={photo.caption ?? "Photo"} /> : <span className="photo-thumb-loading" />}
      {photo.hadGps && <span className="photo-badge" title="The original contains a GPS position">GPS</span>}
    </button>
  );
}

/** Larger view of the report copy, with caption, "in report" and delete. */
function PhotoViewer({
  photo,
  onChange,
  onDelete,
  onClose,
}: {
  photo: Photo;
  onChange: (p: Photo) => void;
  onDelete: () => void;
  onClose: () => void;
}) {
  const url = useBlobUrl(() => api.photoReportImage(photo.id), [photo.id]);
  const [caption, setCaption] = useState(photo.caption ?? "");
  const [error, setError] = useState<ApiError | null>(null);
  const [saving, setSaving] = useState(false);

  // Escape closes the viewer only (not the selection behind it).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [onClose]);

  const update = async (nextCaption: string, inReport: boolean) => {
    setSaving(true);
    try {
      const updated = await api.updatePhoto(photo.id, nextCaption.trim() || null, inReport);
      onChange(updated);
      setCaption(updated.caption ?? "");
      setError(null);
    } catch (e) {
      setError(asApiError(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="photo-viewer" role="dialog" aria-modal="true" aria-label="Photo" onClick={onClose}>
      <div className="photo-viewer-body" onClick={(e) => e.stopPropagation()}>
        <div className="photo-viewer-image">
          {url ? <img src={url} alt={photo.caption ?? "Photo"} /> : <span className="muted">Loading…</span>}
        </div>
        <div className="photo-viewer-side">
          {error && <ErrorBanner error={error} compact />}
          <label>
            <span className="field-label">Caption</span>
            <input
              className="input photo-caption"
              value={caption}
              maxLength={MAX_CAPTION_CHARS}
              placeholder="e.g. Rack in the server room"
              onChange={(e) => setCaption(e.target.value)}
              onBlur={() => {
                if (caption.trim() !== (photo.caption ?? "")) void update(caption, photo.inReport);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  e.currentTarget.blur();
                }
              }}
            />
          </label>
          <label className="checkbox">
            <input
              type="checkbox"
              checked={photo.inReport}
              disabled={saving}
              onChange={(e) => void update(caption, e.target.checked)}
            />
            Include in the report
          </label>
          <p className="muted small">
            {photo.width} × {photo.height} px
            {photo.takenAt && <> · taken {formatTakenAt(photo.takenAt)}</>}
          </p>
          {photo.hadGps && (
            <p className="small notes-warn">
              The original contains the GPS position where it was taken. It stays in Fresnel's data folder as
              evidence; reports use this downscaled copy, which has no location or other metadata.
            </p>
          )}
          <div className="panel-actions">
            <button type="button" className="btn" onClick={onClose}>
              Close
            </button>
            <ConfirmButton onConfirm={onDelete} title="Delete this photo (the original too)">
              Delete
            </ConfirmButton>
          </div>
        </div>
      </div>
    </div>
  );
}

/** Thumbnails of the photos attached to one floor, point, AP or note pin, with "Add photo". */
export function PhotoStrip({ floorId, target }: { floorId: number; target: PhotoTarget }) {
  const [photos, setPhotos] = useState<Photo[]>([]);
  const [openId, setOpenId] = useState<number | null>(null);
  const [adding, setAdding] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const key = `${target.kind}-${target.id}`;

  useEffect(() => {
    setPhotos([]);
    setOpenId(null);
    setError(null);
    let cancelled = false;
    api
      .listFloorPhotos(floorId)
      .then((all) => !cancelled && setPhotos(all.filter((p) => sameTarget(p.target, target))))
      .catch((e) => !cancelled && setError(asApiError(e)));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [floorId, key]);

  const add = async (e: ChangeEvent<HTMLInputElement>) => {
    const files = [...(e.target.files ?? [])];
    e.target.value = "";
    setError(null);
    for (const [i, file] of files.entries()) {
      setAdding(files.length > 1 ? `Adding ${i + 1} of ${files.length}…` : "Adding…");
      try {
        if (file.size > MAX_PHOTO_BYTES) {
          throw {
            kind: "invalid_input",
            message: `${file.name} is ${Math.round(file.size / (1024 * 1024))} MB; the limit is 25 MB.`,
          } satisfies ApiError;
        }
        const bytes = new Uint8Array(await file.arrayBuffer());
        const photo = await api.importPhoto(target, bytes);
        setPhotos((ps) => [...ps, photo]);
      } catch (err) {
        const apiErr = asApiError(err);
        setError({ ...apiErr, message: files.length > 1 ? `${file.name}: ${apiErr.message}` : apiErr.message });
        break;
      }
    }
    setAdding(null);
  };

  const remove = async (id: number) => {
    try {
      await api.deletePhoto(id);
      setPhotos((ps) => ps.filter((p) => p.id !== id));
      setOpenId(null);
    } catch (e) {
      setError(asApiError(e));
    }
  };

  const open = photos.find((p) => p.id === openId) ?? null;

  return (
    <div className="photo-strip">
      <span className="field-label">
        Photos {photos.length > 0 && <span className="muted">({photos.length})</span>}
      </span>
      {error && <ErrorBanner error={error} compact />}
      <div className="photo-tiles">
        {photos.map((p) => (
          <Thumb key={p.id} photo={p} onOpen={() => setOpenId(p.id)} />
        ))}
        <input ref={input} type="file" accept={ACCEPT} multiple hidden onChange={(e) => void add(e)} />
        <button
          type="button"
          className="photo-add"
          onClick={() => input.current?.click()}
          disabled={adding != null}
          title="JPEG, PNG or WebP, up to 25 MB"
        >
          {adding ?? "+ Add photo"}
        </button>
      </div>
      <p className="muted small">
        Originals are kept unchanged, with their metadata (including GPS location if the camera recorded it).
        Only the downscaled report copies are stripped.
      </p>
      {open && (
        <PhotoViewer
          key={open.id}
          photo={open}
          onChange={(u) => setPhotos((ps) => ps.map((p) => (p.id === u.id ? u : p)))}
          onDelete={() => void remove(open.id)}
          onClose={() => setOpenId(null)}
        />
      )}
    </div>
  );
}
