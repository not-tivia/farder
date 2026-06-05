// App-wide toast notifications. Callable from ANYWHERE — components, hooks, or
// plain functions — via a window CustomEvent (matching the codebase's existing
// CustomEvent pattern), so no React context/provider is needed. The
// <ToastContainer> mounted at the app root listens and renders them.
//
//   import { toast } from "../lib/toast";
//   toast.error("Couldn't save edit");
//   toast.success("Saved");
//   toast.info("Reconnecting…");

export type ToastVariant = "error" | "success" | "info";

export interface ToastEvent {
  id: number;
  message: string;
  variant: ToastVariant;
}

let nextId = 1;

function show(message: string, variant: ToastVariant) {
  window.dispatchEvent(
    new CustomEvent<ToastEvent>("farder:toast", {
      detail: { id: nextId++, message, variant },
    }),
  );
}

export const toast = {
  error: (message: string) => show(message, "error"),
  success: (message: string) => show(message, "success"),
  info: (message: string) => show(message, "info"),
};
