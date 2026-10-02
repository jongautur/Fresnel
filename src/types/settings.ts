// Mirrors fresnel-core settings.rs (camelCase).

export interface Branding {
  technicianName: string | null;
  companyName: string | null;
}

export interface LogoInfo {
  mime: "image/png" | "image/jpeg";
  bytes: number;
  width: number;
  height: number;
}

export interface BrandingInfo extends Branding {
  logo: LogoInfo | null;
}
