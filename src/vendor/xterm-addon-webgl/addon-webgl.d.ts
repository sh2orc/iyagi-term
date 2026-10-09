/**
 * Type declarations for the vendored @xterm/addon-webgl bundle (addon-webgl.js). Mirrors the
 * upstream typings (node_modules/@xterm/addon-webgl/typings/addon-webgl.d.ts) as a local module.
 * MIT License — Copyright (c) The xterm.js authors.
 */
import type { IEvent, ITerminalAddon, Terminal } from "@xterm/xterm";

/** An xterm.js addon that provides hardware-accelerated rendering functionality via WebGL. */
export class WebglAddon implements ITerminalAddon {
  public textureAtlas?: HTMLCanvasElement;
  /** An event that is fired when the renderer loses its canvas context. */
  public readonly onContextLoss: IEvent<void>;
  /** An event that is fired when the texture atlas of the renderer changes. */
  public readonly onChangeTextureAtlas: IEvent<HTMLCanvasElement>;
  /** An event that is fired when a new page is added to the texture atlas. */
  public readonly onAddTextureAtlasCanvas: IEvent<HTMLCanvasElement>;
  /** An event that is fired when a page is removed from the texture atlas. */
  public readonly onRemoveTextureAtlasCanvas: IEvent<HTMLCanvasElement>;
  constructor(preserveDrawingBuffer?: boolean);
  /** Activates the addon. */
  public activate(terminal: Terminal): void;
  /** Disposes the addon. */
  public dispose(): void;
  /** Clears the terminal's texture atlas and triggers a redraw. */
  public clearTextureAtlas(): void;
}
