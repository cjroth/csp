import { useEffect } from "react";
import { Outlet, useNavigate } from "react-router";
import { Sidebar } from "@/components/layout/Sidebar";
import { Toaster } from "@/components/ui/sonner";
import { EngineProvider } from "@/hooks/useEngine";

/** Bridge native tray menu items to in-app UI (spec §6.1). */
function TrayBridge() {
  const navigate = useNavigate();
  useEffect(() => {
    let cleanups: Array<() => void> = [];
    void import("@tauri-apps/api/event").then(({ listen }) => {
      const wire = async (evt: string, fn: (p: unknown) => void) =>
        cleanups.push(await listen(evt, (e) => fn(e.payload)));
      void wire("tray://add-local", () => {
        navigate("/");
        window.dispatchEvent(new CustomEvent("ctx:add-local"));
      });
      void wire("tray://connect-remote", () => {
        navigate("/");
        window.dispatchEvent(new CustomEvent("ctx:connect-remote"));
      });
      void wire("tray://open-folder", (id) => navigate(`/folders/${String(id)}`));
    });
    return () => {
      for (const c of cleanups) c();
      cleanups = [];
    };
  }, [navigate]);
  return null;
}

export function App() {
  return (
    <EngineProvider>
      <TrayBridge />
      <div className="flex h-screen w-screen overflow-hidden text-foreground">
        <Sidebar />
        <main className="relative flex-1 overflow-y-auto">
          <div className="pointer-events-none absolute inset-x-0 top-0 z-10 h-12 bg-gradient-to-b from-background to-transparent" />
          <div className="animate-in fade-in duration-500">
            <Outlet />
          </div>
        </main>
      </div>
      <Toaster richColors closeButton position="bottom-right" />
    </EngineProvider>
  );
}
