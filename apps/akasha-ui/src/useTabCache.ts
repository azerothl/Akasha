/**
 * Simple TTL cache for tab data to avoid refetching on every tab switch.
 * Stale-while-revalidate: use cached data when fresh, refetch in background when expired.
 */

const CACHE_TTL_MS = 45_000; // 45 seconds

type CacheEntry<T> = { data: T; ts: number };

const cache = new Map<string, CacheEntry<unknown>>();

export function getCached<T>(key: string, ttlMs: number = CACHE_TTL_MS): T | null {
  const entry = cache.get(key) as CacheEntry<T> | undefined;
  if (!entry) return null;
  if (Date.now() - entry.ts >= ttlMs) return null;
  return entry.data;
}

export function setCached<T>(key: string, data: T): void {
  cache.set(key, { data, ts: Date.now() });
}

export function isCacheFresh(key: string, ttlMs: number = CACHE_TTL_MS): boolean {
  const entry = cache.get(key);
  if (!entry) return false;
  return Date.now() - entry.ts < ttlMs;
}

export const TAB_CACHE_TTL = CACHE_TTL_MS;
