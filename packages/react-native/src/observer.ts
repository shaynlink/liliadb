import type { DatabaseChangeEvent } from './types.js';

export class DatabaseObserver {
  private readonly listeners = new Map<string, Set<() => void>>();

  subscribe(pattern: string, callback: () => void): () => void {
    let set = this.listeners.get(pattern);
    if (!set) {
      set = new Set();
      this.listeners.set(pattern, set);
    }
    set.add(callback);

    return () => {
      const current = this.listeners.get(pattern);
      if (current) {
        current.delete(callback);
        if (current.size === 0) {
          this.listeners.delete(pattern);
        }
      }
    };
  }

  emit(event: DatabaseChangeEvent): void {
    const targets: string[] = ['*'];

    switch (event.type) {
      case 'kv':
        targets.push(`kv:${event.namespace}`);
        targets.push(`kv:${event.namespace}:${event.key}`);
        break;
      case 'json':
        targets.push(`json:${event.space}`);
        targets.push(`json:${event.space}:${event.id}`);
        break;
      case 'table':
        targets.push(`table:${event.table}`);
        break;
      case 'custom':
        targets.push(`tag:${event.tag}`);
        break;
    }

    const notified = new Set<() => void>();
    for (const target of targets) {
      const set = this.listeners.get(target);
      if (set) {
        for (const cb of set) {
          if (!notified.has(cb)) {
            notified.add(cb);
            try {
              cb();
            } catch (err) {
              console.error('[LiliaDB Observer] Error in subscriber callback:', err);
            }
          }
        }
      }
    }
  }

  clear(): void {
    this.listeners.clear();
  }
}
