import { writable } from 'svelte/store';
import type { ProjectMemory } from '../../types';
import { getMemoryFiles } from '../api';
export const memory = writable<ProjectMemory[]>([]);
export async function refreshMemory() { memory.set(await getMemoryFiles()); }
