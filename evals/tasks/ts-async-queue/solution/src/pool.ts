export async function mapLimit<T, R>(items: T[], limit: number, fn: (item: T, index: number) => Promise<R>): Promise<R[]> {
  if (!Number.isInteger(limit) || limit < 1) throw new RangeError("limit must be a positive integer");
  const results: R[] = new Array(items.length);
  let next = 0;
  let active = 0;
  let failed = false;
  let error: unknown;

  return new Promise<R[]>((resolve, reject) => {
    const launch = () => {
      if (failed) {
        if (active === 0) reject(error);
        return;
      }
      if (next === items.length && active === 0) {
        resolve(results);
        return;
      }
      while (active < limit && next < items.length) {
        const index = next++;
        active++;
        fn(items[index], index).then(
          (value) => {
            results[index] = value;
            active--;
            launch();
          },
          (err) => {
            if (!failed) {
              failed = true;
              error = err;
            }
            active--;
            launch();
          },
        );
      }
    };
    launch();
  });
}
