import { useEffect, useState, type ReactNode } from "react";
import { GlazePortalProvider } from "@/lib/glaze/portal";

const DARK = "(prefers-color-scheme: dark)";

/** The system's dark-mode query, where the web view has one. */
function darkQuery(): MediaQueryList | null {
  return typeof window.matchMedia === "function" ? window.matchMedia(DARK) : null;
}

/** Whether macOS is set to dark, kept up to date; light where it can't be asked. */
function useSystemDark(): boolean {
  const [dark, setDark] = useState(() => darkQuery()?.matches ?? false);
  useEffect(() => {
    const query = darkQuery();
    if (!query) return;
    const changed = (event: { matches: boolean }) => setDark(event.matches);
    query.addEventListener("change", changed);
    return () => query.removeEventListener("change", changed);
  }, []);
  return dark;
}

/**
 * The main window's one Glaze scope (TP-GLZ-01, ADR-016): Glaze's tokens apply inside it, and
 * its menus and dialogs stay inside through the portal provider. Glaze doesn't follow the
 * system's light or dark setting on its own, so this does, as the window always has.
 */
export function GlazeRoot({ children }: { children: ReactNode }) {
  const dark = useSystemDark();
  return (
    <div className="glaze min-h-dvh bg-page text-text-1" data-theme={dark ? "dark" : "light"}>
      <GlazePortalProvider>{children}</GlazePortalProvider>
    </div>
  );
}
