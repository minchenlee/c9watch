import { writable } from 'svelte/store';
import type { SubscriptionUsage } from '../../subscription-usage';
import { getSubscriptionUsage } from '../api';
export const usage = writable<SubscriptionUsage[]>([]);
export async function refreshUsage() { usage.set(await getSubscriptionUsage()); }
