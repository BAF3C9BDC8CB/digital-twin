/**
 * Utility functions for data formatting.
 */

export function formatCurrency(amount: number, locale: string = "zh-CN"): string {
    const formatted = amount.toFixed(2);
    return `¥${formatted}`;
}

export async function fetchUser(id: string): Promise<User> {
    const response = await api.get(`/users/${id}`);
    return response.data as User;
}

export interface User {
    id: string;
    name: string;
    email: string;
}

type Callback = (err: Error | null, data: string) => void;

const cache: Map<string, string> = new Map();

export function getCache(key: string): string | undefined {
    return cache.get(key);
}

export function setCache(key: string, value: string): void {
    cache.set(key, value);
}
