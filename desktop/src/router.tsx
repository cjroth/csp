import { createHashRouter } from "react-router";
import { App } from "@/App";
import { FolderDetailPage } from "@/pages/FolderDetailPage";
import { FoldersPage } from "@/pages/FoldersPage";
import { SettingsPage } from "@/pages/SettingsPage";

// HashRouter, not BrowserRouter: under Tauri the app is served from a custom
// protocol with no SPA history fallback, so path reloads/deep links would
// 404. Hash routing works identically in the browser and the webview.
export const router = createHashRouter([
  {
    path: "/",
    element: <App />,
    children: [
      { index: true, element: <FoldersPage /> },
      { path: "folders/:id", element: <FolderDetailPage /> },
      { path: "settings", element: <SettingsPage /> },
    ],
  },
]);
