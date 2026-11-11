/**
 * Copy text to the clipboard; resolves to whether it really got there.
 *
 * `navigator.clipboard` only exists in a secure context (HTTPS or localhost). Allod is often
 * opened over plain HTTP on a LAN address or a `.local` name, where browsers leave it
 * undefined, so fall back to the selection-based copy that works there too.
 */
export async function copyText(text: string): Promise<boolean> {
  if (window.isSecureContext && navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // Permission refused or the document lost focus: try the legacy path.
    }
  }
  return legacyCopy(text);
}

function legacyCopy(text: string): boolean {
  if (typeof document.execCommand !== "function") return false;
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  // Off-screen but still selectable; font size keeps iOS from zooming in.
  area.style.cssText = "position:fixed;top:0;left:-9999px;opacity:0;font-size:16px";
  const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  // Inside an open <dialog> only the dialog's subtree can take focus and selection.
  const host = active?.closest("dialog") ?? document.body;
  host.appendChild(area);
  try {
    area.select();
    area.setSelectionRange(0, text.length);
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    area.remove();
    active?.focus();
  }
}
