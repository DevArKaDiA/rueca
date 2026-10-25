function bump(n: number): number {
  return n + 1;
}

function reset(): number {
  return 0;
}

export function makeCounter() {
  let n = reset();
  const step = (): number => {
    n = bump(n);
    return n;
  };
  return {
    next: (): number => {
      return step();
    },
    peek(): number {
      return n;
    },
  };
}

export function useCounter(): number {
  const c = makeCounter();
  c.next();
  return c.peek();
}
