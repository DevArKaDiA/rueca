function leaf(n: number): number {
  return n + 1;
}

export function outer(n: number): number {
  function inner(k: number): number {
    return leaf(k) * 2;
  }
  const twice = (k: number): number => {
    return inner(inner(k));
  };
  return twice(n);
}
