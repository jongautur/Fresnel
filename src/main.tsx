import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { installErrorForwarding } from "./lib/errorForwarding";
import "./styles/app.css";
import "./styles/tools.css";

installErrorForwarding();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary page>
      <App />
    </ErrorBoundary>
  </StrictMode>,
);
