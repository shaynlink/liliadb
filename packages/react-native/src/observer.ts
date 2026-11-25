// Listeners of change topics (see `changes.ts`).

/** The topic every change reaches. */
export const ANY_CHANGE = "*";

export class DatabaseObserver {
  private readonly listeners = new Map<string, Set<() => void>>();

  /** Calls `listener` after each write that changes one of `topics`; returns the unsubscribe. */
  subscribe(topics: string | readonly string[], listener: () => void): () => void {
    const names = typeof topics === "string" ? [topics] : [...topics];
    for (const topic of names) {
      let set = this.listeners.get(topic);
      if (!set) {
        set = new Set();
        this.listeners.set(topic, set);
      }
      set.add(listener);
    }
    return () => {
      for (const topic of names) {
        const set = this.listeners.get(topic);
        set?.delete(listener);
        if (set?.size === 0) this.listeners.delete(topic);
      }
    };
  }

  /**
   * Calls, once each, the listeners of `topics` and of `ANY_CHANGE`; every listener when
   * `topics` is `undefined`. A listener that throws does not stop the others: its error is
   * thrown again on its own, after them.
   */
  emit(topics: readonly string[] | undefined): void {
    if (topics?.length === 0) return;
    const sets = topics === undefined ? [...this.listeners.values()] : [...topics, ANY_CHANGE].map((topic) => this.listeners.get(topic));
    // A snapshot, each listener once: a listener may subscribe or unsubscribe while they run.
    const listeners = new Set(sets.flatMap((set) => (set ? [...set] : [])));
    for (const listener of listeners) {
      try {
        listener();
      } catch (error) {
        queueMicrotask(() => {
          throw error;
        });
      }
    }
  }

  clear(): void {
    this.listeners.clear();
  }
}
