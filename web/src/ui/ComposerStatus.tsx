import { turnClass, type TurnState } from "./turnState.ts";
import "./ComposerStatus.css";

/** A session condition that replaces the turn readout (docs/web-client.md). */
export interface ComposerStatusMessage {
  text: string;
  error?: boolean;
  detail?: string;
  action?: {
    label: string;
    pending: boolean;
    onClick: () => void;
  };
}

export function ComposerStatus({
  turn,
  message,
}: {
  turn: TurnState;
  message?: ComposerStatusMessage | null;
}) {
  if (!message)
    return (
      <span className={turnClass(turn.kind)} title={turn.title}>
        {turn.text}
      </span>
    );
  return (
    <div
      className={`composer-status${message.error ? " is-error" : ""}`}
      role={message.error ? "alert" : "status"}
    >
      <div className="composer-status-heading">
        <span>{message.text}</span>
        {message.action && (
          <button
            type="button"
            disabled={message.action.pending}
            onClick={message.action.onClick}
          >
            {message.action.label}
          </button>
        )}
      </div>
      {message.detail && <p>{message.detail}</p>}
    </div>
  );
}
