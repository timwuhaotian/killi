export interface RetryOptions {
  /** Total number of attempts, including the first. */
  attempts: number;
  /** Wait before the second attempt, in milliseconds. */
  delayMs: number;
  /** Each later wait is the previous one times this. Defaults to 2. */
  factor?: number;
  /** Return false to stop retrying and throw this error at once. */
  shouldRetry?: (error: unknown) => boolean;
  /** Injected for tests; defaults to a real timer. */
  sleep?: (ms: number) => Promise<void>;
}

const realSleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/**
 * Calls `fn` until it succeeds, waiting between attempts. Throws the last error once every
 * attempt has failed.
 */
export async function retry<T>(fn: (attempt: number) => Promise<T>, options: RetryOptions): Promise<T> {
  const sleep = options.sleep ?? realSleep;
  let delay = options.delayMs;
  let lastError: unknown = new Error("retry: no attempts were made");
  for (let attempt = 1; attempt <= options.attempts; attempt++) {
    try {
      return await fn(attempt);
    } catch (error) {
      lastError = error;
      if (options.shouldRetry && !options.shouldRetry(error)) throw error;
      if (attempt === options.attempts) break;
      await sleep(delay);
      delay *= options.factor ?? 2;
    }
  }
  throw lastError;
}
