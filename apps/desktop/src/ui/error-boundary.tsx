import { Component, type ReactNode } from "react";
import { BrandMark } from "./brand";
import { Button } from "./button";
import { Icon } from "./icon";

/** Catches a throw while the window draws and shows a plain message with Reload instead of a
 * blank window. React logs the error itself, so its text stays off the screen.
 * Input: the window's content. Output: the content, or the message once anything threw. */
export class AppErrorBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return <main id="main" className="full-state">
      <div className="box" role="alert">
        <BrandMark />
        <Icon name="circle-alert" size={28} />
        <h1>This window stopped working</h1>
        <p className="secondary">Something went wrong while drawing it. Your projects and agents aren't affected. Reload to bring it back. Anything you typed and didn't save will be lost.</p>
        <span className="row"><Button variant="primary" icon="refresh-cw" onClick={() => window.location.reload()}>Reload</Button></span>
      </div>
    </main>;
  }
}
