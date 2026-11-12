/**
 * Session writes, in order. Each `session_write` is its own async IPC call and
 * the backend runs them as separate tasks, so two keystrokes sent close together
 * can reach the session swapped. `OrderedWriter` sends one write per session at
 * a time; what arrives meanwhile is joined into the next write. Different
 * sessions don't wait for each other. Pure (the write function is passed in).
 */
export class OrderedWriter {
  private q = new Map<string, { busy: boolean; pending: string; waiters: { ok: () => void; err: (e: unknown) => void }[]; idle: (() => void)[] }>();
  private readonly write: (session: string, data: string) => Promise<unknown>;
  constructor(write: (session: string, data: string) => Promise<unknown>) { this.write = write; }

  /** Queue `data` for `session`; resolves once it has been written (rejects if that write failed). */
  send(session: string, data: string): Promise<void> {
    let s = this.q.get(session);
    if (!s) { s = { busy: false, pending: "", waiters: [], idle: [] }; this.q.set(session, s); }
    const st = s;
    const done = new Promise<void>((ok, err) => st.waiters.push({ ok, err }));
    st.pending += data;
    if (!st.busy) void this.pump(session, st);
    return done;
  }

  private async pump(session: string, st: { busy: boolean; pending: string; waiters: { ok: () => void; err: (e: unknown) => void }[]; idle: (() => void)[] }) {
    st.busy = true;
    while (st.pending) {
      const data = st.pending, waiters = st.waiters;
      st.pending = ""; st.waiters = [];
      try { await this.write(session, data); waiters.forEach((w) => w.ok()); }
      catch (e) { waiters.forEach((w) => w.err(e)); }
    }
    st.busy = false;
    const idle = st.idle; st.idle = [];
    this.q.delete(session);
    idle.forEach((f) => f());
  }

  /** Resolves when nothing is queued or in flight (tests). */
  idle(): Promise<void> {
    const busy = [...this.q.values()].filter((s) => s.busy);
    return Promise.all(busy.map((s) => new Promise<void>((r) => s.idle.push(r)))).then(() => {});
  }
}
