/**
 * React context exposing the controller (and only the controller) to the
 * component tree. The controller, registry, and client live outside React —
 * components never own session lifetimes (04-ui.md §4).
 */

import { createContext, useContext } from "react";
import type { SessionController } from "../features/terminal/sessionController";

export const ControllerContext = createContext<SessionController | null>(null);

export function useController(): SessionController {
  const controller = useContext(ControllerContext);
  if (!controller) throw new Error("ControllerContext is not provided");
  return controller;
}
