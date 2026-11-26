import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@/components/glaze/styles.css";
import { App } from "./App";
import { GlazeRoot } from "./lib/GlazeRoot";

const root = document.getElementById("root");
if (!root) throw new Error("Corral: #root element missing from index.html");

createRoot(root).render(
  <StrictMode>
    <GlazeRoot>
      <App />
    </GlazeRoot>
  </StrictMode>,
);
