/**
 * Runs `fn` over `items` with at most `limit` calls in flight at once.
 *
 * Resolves with the results in the same order as `items`. If any call rejects, the returned
 * promise rejects with the first error, after the calls already started have settled; no new
 * calls start after a failure.
 */
export async function mapLimit<T, R>(items: T[], limit: number, fn: (item: T, index: number) => Promise<R>): Promise<R[]> {
  if (!Number.isInteger(limit) || limit < 1) throw new RangeError("limit must be a positive integer");
  const results: R[] = [];
  let next = 0;
  let active = 0;
  let failed: unknown = undefined;

  return new Promise<R[]>((resolve, reject) => {
    const launch = () => {
      if (failed !== undefined) {
        if (active === 0) reject(failed);
        return;
      }
      if (next === items.length && active === 0) {
        resolve(results);
        return;
      }
      while (active < limit && next < items.length) {
        const index = next++;
        active++;
        fn(items[index], index).then((value) => {
          results.push(value);
          active--;
          launch();
        }, (error) => {
          if (failed === undefined) failed = error;
          launch();
        });
      }
    };
    launch();
  });
}
