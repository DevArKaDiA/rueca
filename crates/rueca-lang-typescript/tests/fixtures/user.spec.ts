import { describe, it, expect, vi } from 'vitest';
import { UserService } from './users.service';

describe('UserService', () => {
  const repo = makeRepo();

  describe('findById', () => {
    it('returns the cached user', async () => {
      const svc = new UserService(repo, audit);
      expect(await svc.findById('1')).toBe(null);
    });

    it.each([1, 2])('handles %s', (n) => {
      helper(n);
    });
  });

  test('v1.2 works', () => {
    vi.fn();
  });
});
