import { Component, Fragment, type ErrorInfo, type ReactNode } from "react";
import {
  deriveErrorBoundaryState,
  formatRenderErrorEvent,
  initialErrorBoundaryState,
  resetErrorBoundaryState,
  retryErrorBoundaryState,
  type ErrorBoundaryState,
} from "./errorBoundaryModel.ts";
import { TraceIdChip } from "./TraceIdChip.tsx";

interface ErrorBoundaryProps {
  readonly children: ReactNode;
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = initialErrorBoundaryState();

  static getDerivedStateFromError(error: unknown): Pick<ErrorBoundaryState, "error" | "traceId"> {
    return deriveErrorBoundaryState(error);
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    const traceId = this.state.traceId ?? deriveErrorBoundaryState(error).traceId;
    console.error(JSON.stringify(formatRenderErrorEvent(traceId, error, info.componentStack ?? null)));
    if (this.state.traceId !== traceId) this.setState({ traceId });
  }

  private retry = (): void => {
    this.setState((state) => retryErrorBoundaryState(state));
  };

  private reset = (): void => {
    this.setState((state) => resetErrorBoundaryState(state));
  };

  render(): ReactNode {
    const { error, traceId } = this.state;
    if (error && traceId) {
      return (
        <div className="error-boundary-fallback" role="alert">
          <p className="error-boundary-kicker">工作台界面出现问题</p>
          <h1>这一页没能画出来</h1>
          <p className="error-boundary-message">{error.message || "渲染时发生未处理的异常。"}</p>
          <TraceIdChip traceId={traceId} />
          <div className="error-boundary-actions">
            <button type="button" className="error-boundary-btn" onClick={this.retry}>重试</button>
            <button type="button" className="error-boundary-btn error-boundary-btn-primary" onClick={this.reset}>重置</button>
          </div>
        </div>
      );
    }
    return <Fragment key={this.state.resetKey}>{this.props.children}</Fragment>;
  }
}
