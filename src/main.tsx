import ReactDOM from "react-dom/client";
import { Component, type ReactNode } from "react";
import App from "./App";
import "./App.css";

/** 错误边界：任何渲染错误显示错误信息，避免整屏黑屏 */
class ErrorBoundary extends Component<{ children: ReactNode }, { err: string }> {
  state = { err: "" };
  static getDerivedStateFromError(e: unknown) {
    return { err: e instanceof Error ? `${e.message}\n${e.stack ?? ""}` : String(e) };
  }
  render() {
    if (this.state.err) {
      return (
        <div style={{ padding: 24, fontFamily: "system-ui, sans-serif", background: "#0d1117", color: "#e6edf3", minHeight: "100vh" }}>
          <h2>界面渲染出错</h2>
          <pre style={{ whiteSpace: "pre-wrap", color: "#ffb3ad", fontSize: 12 }}>{this.state.err}</pre>
          <button
            style={{ marginTop: 12, padding: "6px 14px", cursor: "pointer" }}
            onClick={() => this.setState({ err: "" })}
          >
            重试
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <ErrorBoundary>
    <App />
  </ErrorBoundary>,
);
