import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./index.css";

// Follow the system colour scheme.
const media = window.matchMedia?.("(prefers-color-scheme: dark)");
const applyTheme = () => document.documentElement.classList.toggle("dark", !!media?.matches);
applyTheme();
media?.addEventListener?.("change", applyTheme);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
