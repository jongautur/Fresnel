# Fresnel identity

The mark depicts a radio path between two endpoints, enclosed by Fresnel-zone
contours. Blue is the outer zone; teal is the inner contour and endpoints.
On white, the outer zone is charcoal for contrast. Endpoint dots are drawn
without background-colour knockout rings, so transparent marks work on any
background.

## Assets

- `logo-dark.svg`: transparent outlined wordmark for charcoal backgrounds.
- `logo-light.svg`: transparent outlined wordmark for white backgrounds.
- `logo-monochrome.svg`: outlined wordmark using `currentColor` (black by
  default). Inline the SVG to inherit surrounding text colour; an `<img>`
  does not inherit its parent's CSS colour.
- `mark.svg`: transparent blue/teal symbol, tightly cropped.
- `mark-small.svg`: transparent simplified blue/teal symbol for small sizes.
- `mark-light.svg`: transparent charcoal/teal symbol for white backgrounds.
- `mark-monochrome.svg`: transparent symbol using `currentColor`.
- `social.png`: 1280 × 640 social image.
- `social.svg`: editable, outlined source for the social image.
- `../../src-tauri/icons/app-icon.svg`: primary app icon, 1024 × 1024.
- `../../src-tauri/icons/app-icon-small.svg`: simplified dark app icon.
- `../../src-tauri/icons/app-icon-light.svg`: white app icon.
- `../../src-tauri/icons/app-icon-light-small.svg`: simplified white app icon,
  with larger endpoint dots for contrast.

Wordmark lettering is Ubuntu Sans Semibold converted to SVG paths. The
symbol's visible height is 89% of the lettering's visible height. No font
installation is required. All SVGs are self-contained; they have no external
resources or embedded raster images.

App icons retain the rounded square with a 192-unit corner radius at 1024.
The outer ellipse is `rx=390, ry=250`, with endpoints slightly inset to
preserve padding. The full symbol spans about 82% of the tile width and 55%
of its height; the small variant has a heavier outer stroke. Use the small
variant for 16–32 px app icons and the full variant at 48 px and above. SVGs
do not automatically switch artwork when resized.

Open `preview.html` for dark/white wordmarks, transparent marks, and both
icon variants at actual sizes of 16, 24, 32, 48 and 64 px. Rasterized proofs
were inspected at all five sizes.

## App integration

- `src-tauri/icons/`: `icon.png` (512), `128x128.png`, `128x128@2x.png` are
  rendered from `app-icon.svg`; `32x32.png` from `app-icon-small.svg`.
  `icon.ico` holds 16, 24 and 32 px from `app-icon-small.svg` and 48, 64 and
  256 px from `app-icon.svg`. To regenerate: run `npx tauri icon` on each SVG
  into a scratch folder, copy those files, and rebuild the ICO from both.
- The app's sidebar draws `mark-small.svg` inline (`BrandMark` in
  `src/components/Icons.tsx`).
- The README header uses `logo-dark.svg` / `logo-light.svg`.
