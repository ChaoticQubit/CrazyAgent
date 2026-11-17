import { useState } from "react";
import { readTerminalInput, readTerminalRenderer, saveTerminalInput, saveTerminalRenderer } from "../../state";
import { INPUT_CHOICES, type InputChoice } from "../team/terminal-io";
import { RENDERER_CHOICES, type RendererChoice } from "../team/terminal-renderer";
import { Segmented, SettingLine, SettingSection } from "../../ui";

/** The Terminal input and Terminal renderer lines, each read from and saved to its one stored
 * choice, so Settings > Terminal and Profile's Appearance show and change the same setting.
 * Output: the two lines. */
export function TerminalLines() {
  const [input, setInput] = useState<InputChoice>(readTerminalInput);
  const [renderer, setRenderer] = useState<RendererChoice>(readTerminalRenderer);
  return <>
    <SettingLine label="Terminal input" help="Where you type to the master and every agent, on every screen: straight into the terminal, in a message box under it that sends your text as one message, or both. Kept in this app only.">
      <Segmented label="Terminal input" value={input} onValue={(value) => { setInput(value); saveTerminalInput(value); }} options={INPUT_CHOICES} />
    </SettingLine>
    <SettingLine label="Terminal renderer" help="How every terminal draws its text. GPU draws on the graphics card, except on a software graphics driver, without WebGL2 or after the graphics card reset. Automatic also stays on standard drawing once a start on the graphics card failed, until you change this. Standard draws text rows without the graphics card. GPU is the default on macOS and Windows, Standard on Linux. A change applies to open terminals at once. Kept in this app only.">
      <Segmented label="Terminal renderer" value={renderer} onValue={(value) => { setRenderer(value); saveTerminalRenderer(value); }} options={RENDERER_CHOICES} />
    </SettingLine>
  </>;
}

/** Settings > App > Terminal: where you type to agents and how every terminal draws, the same
 * choices Profile's Appearance holds (V243-TERMINAL-SETTINGS-PLACE). Output: the section. */
export function TerminalSettings() {
  return <div className="stack">
    <div className="settings-top"><div><h2>Terminal</h2><p>Where you type to agents and how every terminal draws its text.</p></div></div>
    <SettingSection title="Terminal"><TerminalLines /></SettingSection>
  </div>;
}
