/** A least-recently-used cache: when full, adding a new key evicts the key used longest ago. */
export class LruCache<K, V> {
  private readonly capacity: number;
  private readonly map = new Map<K, V>();

  constructor(capacity: number) {
    if (!Number.isInteger(capacity) || capacity < 1) {
      throw new RangeError("capacity must be a positive integer");
    }
    this.capacity = capacity;
  }

  /** Returns the value and marks the key as most recently used. */
  get(key: K): V | undefined {
    return this.map.get(key);
  }

  /** Adds or updates a key, marking it as most recently used. */
  set(key: K, value: V): void {
    if (this.map.size >= this.capacity) {
      const oldest = this.map.keys().next().value as K;
      this.map.delete(oldest);
    }
    this.map.set(key, value);
  }

  has(key: K): boolean {
    return this.map.has(key);
  }

  get size(): number {
    return this.map.size;
  }

  /** Keys from least to most recently used. */
  keys(): K[] {
    return [...this.map.keys()];
  }
}
