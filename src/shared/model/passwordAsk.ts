import { create } from "zustand";

/// The profile password dialog (#127), asked from anywhere like `confirmModal`;
/// the PasswordHost widget draws it. What is typed lives in the host while it is
/// open and goes only to `attempt`: nothing here keeps it.

/** "ask": the password (a start, the editor). "set": a new one twice. "change": the
 *  current one and a new one twice. "remove": the current one. */
export type PasswordKind = "ask" | "set" | "change" | "remove";
/** `password` is the current one (ask, change, remove); `newPassword` the one to set. */
export type PasswordFields = { password: string; newPassword: string };

export type PasswordReq = {
  id: number;
  kind: PasswordKind;
  title: string;
  message?: string;
  confirmLabel: string;
  /** Runs with what was typed: resolving closes the dialog with its value, a throw keeps it open and says why. */
  attempt: (fields: PasswordFields) => Promise<unknown>;
  /** Cancel pressed while `attempt` runs (a start on its way): ends it. */
  onBusyCancel?: () => void;
  done: (answer: { value: unknown } | null) => void;
};

type State = { req: PasswordReq | null };
export const usePasswordAsk = create<State>(() => ({ req: null }));

let nextId = 1;
/** One dialog at a time: a second ask waits for the first to close. */
let queue: Promise<unknown> = Promise.resolve();

/** Asks for a profile's password and runs `attempt` with it, as often as it fails;
 *  answers `{ value }` from the attempt that went through, or null on Cancel. */
export function askPassword<T>(opts: {
  kind: PasswordKind;
  title: string;
  message?: string;
  confirmLabel: string;
  attempt: (fields: PasswordFields) => Promise<T>;
  onBusyCancel?: () => void;
}): Promise<{ value: T } | null> {
  const turn = queue.then(() => new Promise<{ value: T } | null>((resolve) => {
    const id = nextId++;
    let settled = false;
    usePasswordAsk.setState({
      req: {
        ...opts,
        id,
        // Once: an attempt that lands after Cancel changes nothing, and never closes the next ask.
        done: (answer) => {
          if (settled) return;
          settled = true;
          if (usePasswordAsk.getState().req?.id === id) usePasswordAsk.setState({ req: null });
          resolve(answer as { value: T } | null);
        },
      },
    });
  }));
  queue = turn.catch(() => {});
  return turn;
}
