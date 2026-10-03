// Asking App to show a page from anywhere (e.g. Tools › iperf3 → Settings).
export type PageId = "live" | "networks" | "survey" | "tools" | "settings";

export const NAVIGATE_EVENT = "fresnel:navigate";

export interface NavigateDetail {
  page: PageId;
  /** An element id to scroll to on that page. */
  anchor?: string;
}

export function navigate(page: PageId, anchor?: string) {
  window.dispatchEvent(new CustomEvent<NavigateDetail>(NAVIGATE_EVENT, { detail: { page, anchor } }));
}
