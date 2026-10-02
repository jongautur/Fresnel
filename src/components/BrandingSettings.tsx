import { useEffect, useRef, useState, type ChangeEvent } from "react";
import { api, asApiError } from "../api/tauri";
import type { ApiError } from "../types/wifi";
import type { BrandingInfo } from "../types/settings";
import { ErrorBanner } from "./ErrorBanner";
import { KeyValueGrid, KV } from "./KeyValue";

/** Matches the backend's limits (checked there). */
const MAX_NAME_CHARS = 120;
const MAX_LOGO_BYTES = 2 * 1024 * 1024;

/** Technician, company and logo shown on report covers. Stored by the backend in the app data folder. */
export function BrandingSettings() {
  const [info, setInfo] = useState<BrandingInfo | null>(null);
  const [technician, setTechnician] = useState("");
  const [company, setCompany] = useState("");
  const [logoUrl, setLogoUrl] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [saving, setSaving] = useState(false);
  const [savedNote, setSavedNote] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);

  const apply = (b: BrandingInfo) => {
    setInfo(b);
    setTechnician(b.technicianName ?? "");
    setCompany(b.companyName ?? "");
  };

  useEffect(() => {
    api.getBranding().then(apply, (e) => setError(asApiError(e)));
  }, []);

  // Logo preview as a blob URL (the webview can't read the file itself).
  const logoKey = info?.logo ? `${info.logo.bytes}:${info.logo.width}x${info.logo.height}` : null;
  useEffect(() => {
    setLogoUrl(null);
    if (!logoKey || !info?.logo) return;
    let url: string | null = null;
    let cancelled = false;
    const mime = info.logo.mime;
    api.brandingLogo().then(
      (buf) => {
        if (cancelled || buf.byteLength === 0) return;
        url = URL.createObjectURL(new Blob([buf], { type: mime }));
        setLogoUrl(url);
      },
      (e) => !cancelled && setError(asApiError(e)),
    );
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [logoKey]);

  const dirty = info != null && (technician.trim() !== (info.technicianName ?? "") || company.trim() !== (info.companyName ?? ""));

  const saveNames = async () => {
    setSaving(true);
    try {
      apply(await api.setBranding({ technicianName: technician.trim() || null, companyName: company.trim() || null }));
      setError(null);
      setSavedNote(true);
    } catch (e) {
      setError(asApiError(e));
    } finally {
      setSaving(false);
    }
  };

  const chooseLogo = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    if (file.size > MAX_LOGO_BYTES) {
      setError({ kind: "invalid_input", message: `The logo is ${(file.size / (1024 * 1024)).toFixed(1)} MB; the limit is 2 MB.` });
      return;
    }
    setSaving(true);
    try {
      const b = await api.setBrandingLogo(new Uint8Array(await file.arrayBuffer()));
      setInfo(b);
      setError(null);
    } catch (err) {
      setError(asApiError(err));
    } finally {
      setSaving(false);
    }
  };

  const removeLogo = async () => {
    setSaving(true);
    try {
      setInfo(await api.clearBrandingLogo());
      setError(null);
    } catch (e) {
      setError(asApiError(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="card">
      <header className="card-header">
        <h2>Report branding</h2>
      </header>
      {error && (
        <div className="pad">
          <ErrorBanner error={error} compact />
        </div>
      )}
      <KeyValueGrid>
        <KV k="Technician">
          <input
            className="input"
            value={technician}
            maxLength={MAX_NAME_CHARS}
            placeholder="Your name"
            disabled={!info || saving}
            onChange={(e) => {
              setTechnician(e.target.value);
              setSavedNote(false);
            }}
          />
        </KV>
        <KV k="Company">
          <input
            className="input"
            value={company}
            maxLength={MAX_NAME_CHARS}
            placeholder="Company name"
            disabled={!info || saving}
            onChange={(e) => {
              setCompany(e.target.value);
              setSavedNote(false);
            }}
          />
          <div className="panel-actions branding-save">
            <button type="button" className="btn" disabled={!dirty || saving} onClick={() => void saveNames()}>
              Save
            </button>
            {savedNote && !dirty && <span className="muted small">Saved.</span>}
          </div>
        </KV>
        <KV k="Logo">
          <div className="branding-logo">
            {logoUrl && <img src={logoUrl} alt="Logo" />}
            <input ref={fileInput} type="file" accept=".png,.jpg,.jpeg,image/png,image/jpeg" hidden onChange={(e) => void chooseLogo(e)} />
            <div className="panel-actions">
              <button type="button" className="btn" disabled={!info || saving} onClick={() => fileInput.current?.click()}>
                {info?.logo ? "Replace logo…" : "Choose logo…"}
              </button>
              {info?.logo && (
                <button type="button" className="btn" disabled={saving} onClick={() => void removeLogo()}>
                  Remove
                </button>
              )}
            </div>
          </div>
          <div className="field-hint">
            PNG or JPEG, at most 2 MB. SVG isn't accepted: the report must not contain anything that can run. Shown with
            the names on the cover of every report.
          </div>
        </KV>
      </KeyValueGrid>
    </section>
  );
}
